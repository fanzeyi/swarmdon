use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::Query;
use axum::headers::Cookie;
use axum::headers::Header;
use axum::headers::SetCookie;
use axum::response::Html;
use axum::TypedHeader;
use axum::{extract::State, response::Redirect, Form};
use http::HeaderValue;
use mastodon_async::{apps::AppBuilder, registration::Registered, Registration};
use serde::Deserialize;
use simple_cookie::decode_cookie;
use simple_cookie::encode_cookie;
use url::Url;

use crate::state::AppState;
use crate::swarm::{SwarmCheckin, SwarmPush};
use crate::utils::ResultExt;

fn set_cookie(signing_key: &[u8; 32], key: &'static str, value: String) -> Result<SetCookie> {
    let encoded = format!(
        "{}={}; Path=/; HttpOnly; Max-Age=604800; Secure",
        key,
        encode_cookie(signing_key, key, value)
    );
    let cookies = vec![HeaderValue::from_str(&encoded)?];
    let mut cookies = cookies.iter();
    Ok(SetCookie::decode(&mut cookies)?)
}

fn get_cookie(cookie: &Cookie, signing_key: &[u8; 32], key: &'static str) -> Option<String> {
    cookie
        .get(key)
        .map(|value| decode_cookie(signing_key, key, value))
        .flatten()
        .map(|value| String::from_utf8_lossy(&value).into_owned())
}

pub async fn get_home() -> Html<&'static str> {
    Html(include_str!("../static/home.html"))
}

#[derive(Deserialize)]
pub struct HomeForm {
    instance_url: String,
}

pub async fn post_home(
    State(state): State<Arc<AppState>>,
    Form(form): Form<HomeForm>,
) -> Result<(TypedHeader<SetCookie>, Redirect), String> {
    let mut instance_url = form.instance_url;

    if !instance_url.starts_with("https:") {
        instance_url = format!("https://{}", instance_url);
    }

    let instance_url = Url::parse(&instance_url).from_err()?;

    if instance_url.scheme() != "https" {
        return Err("instance_url must be https".into());
    }

    let registered =
        get_or_create_registration(&state.db, &state.app_builder, instance_url.clone())
            .await
            .from_err()?;

    let set_cookie =
        set_cookie(&state.signing_key, "instance_url", instance_url.to_string()).from_err()?;

    Ok((
        TypedHeader(set_cookie),
        Redirect::to(&registered.authorize_url().from_err()?),
    ))
}

pub async fn get_or_create_registration<T: Into<String>>(
    db: &crate::model::Database,
    app: &AppBuilder<'static>,
    instance_url: T,
) -> Result<Registered> {
    let instance_url = instance_url.into();
    match db.get_registration(&instance_url) {
        Ok(Some(registration)) => return registration.into_registered(),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                instance_url,
                ?error,
                "error happened when getting registration"
            );
        }
    }

    let registered = Registration::new(instance_url.clone())
        .register(app.clone())
        .await?;
    db.save_registration(instance_url, registered.clone())?;
    Ok(registered)
}

pub async fn get_mastodon_callback(
    State(state): State<Arc<AppState>>,
    TypedHeader(cookie): TypedHeader<Cookie>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<(TypedHeader<SetCookie>, Redirect), String> {
    let Some(code) = params.get("code") else {
        return Err("missing code".into());
    };

    let Some(instance_url) = get_cookie(&cookie, &state.signing_key, "instance_url") else {
        return Err("missing instance_url cookie".into());
    };

    let Ok(Some(registration)) = state.db.get_registration(&instance_url) else {
        return Err("missing registration".into());
    };
    let registered = registration.into_registered().from_err()?;
    let mastodon = registered.complete(&code).await.from_err()?;
    let account = mastodon.verify_credentials().await.from_err()?;

    let _user = match state
        .db
        .get_mastodon_user(&instance_url, &account.id.to_string())
        .from_err()?
    {
        Some(user) => user,
        None => state
            .db
            .create_user(
                &instance_url,
                &account.id.to_string(),
                mastodon.data.clone(),
            )
            .from_err()?,
    };

    let cookie = set_cookie(
        &state.signing_key,
        "user",
        format!("{}|{}", instance_url, account.id.to_string()),
    )
    .from_err()?;

    Ok((TypedHeader(cookie), Redirect::to("/swarm")))
}

pub async fn get_swarm(
    State(state): State<Arc<AppState>>,
    TypedHeader(cookie): TypedHeader<Cookie>,
) -> Result<Redirect, String> {
    let Some(user_id) = get_cookie(&cookie, &state.signing_key, "user") else {
        return Err("missing user cookie".into());
    };
    let Some((instance_url, mastodon_id)) = user_id.split_once('|') else {
        return Err("invalid user cookie".into());
    };
    let Ok(_user) = state.db.get_mastodon_user(instance_url, mastodon_id) else {
        return Err("invalid user".into());
    };

    let url = state.swarm.get_authenticate_url();
    Ok(Redirect::to(url.as_str()))
}

pub async fn get_swarm_callback(
    State(state): State<Arc<AppState>>,
    TypedHeader(cookie): TypedHeader<Cookie>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<String, String> {
    let Some(code) = params.get("code") else {
        return Err("missing code".into());
    };
    let Some(user_id) = get_cookie(&cookie, &state.signing_key, "user") else {
        return Err("missing user cookie".into());
    };
    let Some((instance_url, mastodon_id)) = user_id.split_once('|') else {
        return Err("invalid user cookie".into());
    };
    let Ok(Some(mut user)) = state.db.get_mastodon_user(instance_url, mastodon_id) else {
        return Err("invalid user".into());
    };

    let user_api = state.swarm.get_access_token(code).await.from_err()?;
    let swarm_user = user_api.get_me().await.from_err()?;
    tracing::debug!(?swarm_user, "swarm user");
    user.swarm_id = swarm_user.id.clone();
    user.swarm_access_token = user_api.access_token.clone();
    state
        .db
        .user
        .insert(
            format!("{}:{}", instance_url, mastodon_id),
            bincode::serialize(&user).from_err()?,
        )
        .from_err()?;
    state
        .db
        .swarm_mapping
        .insert(
            swarm_user.id,
            format!("{}:{}", instance_url, mastodon_id).into_bytes(),
        )
        .from_err()?;

    Ok("done!".into())
}

pub async fn post_swarm_push(
    State(state): State<Arc<AppState>>,
    Form(SwarmPush { checkin, secret }): Form<SwarmPush>,
) -> Result<(), String> {
    tracing::debug!(%checkin, "received push event");
    if secret != state.swarm_push_secret {
        tracing::warn!(%checkin, "received invalid push event");
        return Ok(());
    }

    let checkin: SwarmCheckin = match serde_json::from_str(&checkin) {
        Ok(checkin) => checkin,
        Err(e) => {
            tracing::warn!(%checkin, ?e, "unable to parse the checkin push");
            return Ok(());
        }
    };
    if checkin.private.unwrap_or(false) {
        tracing::info!(checkin=%checkin.id, "checkin is private, skip posting.");
        return Ok(());
    }
    let Some(user) = &checkin.user else {
        tracing::warn!(?checkin, "received push event without an user");
        return Ok(());
    };
    let Ok(Some(user_id)) = state.db.swarm_mapping.get(&user.id) else {
        tracing::warn!(user_id = user.id, "received push event for unknown user");
        return Ok(());
    };
    let user_id = String::from_utf8_lossy(&user_id);
    let Ok(Some(user)) = state.db.get_user(&user_id) else {
        tracing::warn!(user_id = user.id, "received push event for unknown user");
        return Ok(());
    };
    if let Err(e) = user.post_checkin(&checkin, &state.friends_map).await {
        tracing::warn!(?e, checkin=%checkin.id, "unable to post checkin");
        return Ok(());
    }
    tracing::info!(checkin_id = checkin.id, "status posted");
    state.update_last_checkin(&user_id, &checkin.id).await;
    Ok(())
}
