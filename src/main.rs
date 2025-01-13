use std::collections::HashMap;
use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use axum::extract::Query;
use axum::headers::Cookie;
use axum::headers::Header;
use axum::headers::SetCookie;
use axum::response::Html;
use axum::routing::post;
use axum::TypedHeader;
use axum::{extract::State, response::Redirect, routing::get, Form, Router};
use clap::Parser;
use http::HeaderValue;
use mastodon_async::scopes::Read;
use mastodon_async::NewStatus;
use mastodon_async::{
    apps::{App, AppBuilder},
    registration::Registered,
    scopes::{Scopes, Write},
    Registration,
};
use once_cell::sync::OnceCell;
use serde::Deserialize;
use serde::Serialize;
use simple_cookie::decode_cookie;
use simple_cookie::encode_cookie;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use url::Url;

mod model;

#[derive(Debug, Parser)]
struct Flags {
    #[clap(short, long, default_value = "swarmdon.db")]
    database: PathBuf,

    #[clap(short, long, default_value = "127.0.0.1:8000")]
    address: String,

    #[clap(short, long, default_value = "Swarmdon")]
    client_name: String,

    #[clap(short, long, default_value = "https://127.0.0.1:8000")]
    base_url: String,

    #[clap(long)]
    swarm_client_id: String,

    #[clap(long)]
    swarm_client_secret: String,

    #[clap(long)]
    swarm_push_secret: String,
}

impl Flags {
    fn app_builder(&self) -> &'static AppBuilder<'static> {
        static APP: OnceCell<AppBuilder> = OnceCell::new();
        APP.get_or_init(|| {
            let mut builder = App::builder();
            builder
                .client_name(self.client_name.clone())
                .redirect_uris(format!("{}/mastodon/callback", self.base_url))
                .scopes(Scopes::write(Write::Statuses) | Scopes::read(Read::Accounts));
            builder
        })
    }
}

struct AppState {
    flags: Flags,
    db: model::Database,
    signing_key: [u8; 32],
}

async fn get_home() -> Html<&'static str> {
    Html(include_str!("../static/home.html"))
}

#[derive(Deserialize)]
struct HomeForm {
    instance_url: String,
}

pub async fn get_or_create_registration<T: Into<String>>(
    db: &model::Database,
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

trait ResultExt<Ok, Err> {
    fn from_err(self) -> Result<Ok, String>;
}

impl<Ok, Err> ResultExt<Ok, Err> for Result<Ok, Err>
where
    Err: Into<anyhow::Error>,
{
    fn from_err(self) -> Result<Ok, String> {
        self.map_err(|e| e.into().to_string())
    }
}

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

async fn post_home(
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
        get_or_create_registration(&state.db, state.flags.app_builder(), instance_url.clone())
            .await
            .from_err()?;

    let set_cookie =
        set_cookie(&state.signing_key, "instance_url", instance_url.to_string()).from_err()?;

    Ok((
        TypedHeader(set_cookie),
        Redirect::to(&registered.authorize_url().from_err()?),
    ))
}

fn get_cookie(cookie: &Cookie, signing_key: &[u8; 32], key: &'static str) -> Option<String> {
    cookie
        .get(key)
        .map(|value| decode_cookie(signing_key, key, value))
        .flatten()
        .map(|value| String::from_utf8_lossy(&value).into_owned())
}

async fn get_mastodon_callback(
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

async fn get_swarm(
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

    let mut url =
        Url::parse("https://foursquare.com/oauth2/authenticate").expect("invalid swarm url");
    let mut queries = url.query_pairs_mut();

    queries.append_pair("client_id", &state.flags.swarm_client_id);
    queries.append_pair("response_type", "code");
    queries.append_pair(
        "redirect_uri",
        &format!("{}/swarm/callback", state.flags.base_url),
    );
    drop(queries);

    Ok(Redirect::to(&url.to_string()))
}

async fn swarm_get_access_token(
    client_id: &str,
    client_secret: &str,
    redirect_url: &str,
    code: &str,
) -> Result<String> {
    let mut url =
        Url::parse("https://foursquare.com/oauth2/access_token").expect("invalid swarm url");

    {
        let mut queries = url.query_pairs_mut();
        queries.append_pair("client_id", client_id);
        queries.append_pair("client_secret", client_secret);
        queries.append_pair("grant_type", "authorization_code");
        queries.append_pair("redirect_uri", redirect_url);
        queries.append_pair("code", code);
    }

    let response = reqwest::get(url).await?;
    let response = response.json::<serde_json::Value>().await?;
    let access_token = response
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("unable to retrieve access token for swarm"))?;

    Ok(access_token.to_string())
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SwarmUser {
    id: String,
    first_name: String,
    last_name: String,
    handle: String,
}

async fn swarm_api(method: String, access_token: &str) -> Result<serde_json::Value> {
    let url = format!(
        "https://api.foursquare.com/v2{}?v=20220722&oauth_token={}",
        method, access_token
    );

    let response = reqwest::get(url).await?;
    let mut response = response.json::<serde_json::Value>().await?;
    let Some(response) = response.get_mut("response").map(|v| v.take()) else {
        return Err(anyhow::anyhow!("unable to retrieve response for swarm"));
    };
    Ok(response)
}

async fn swarm_get_me(access_token: &str) -> Result<SwarmUser> {
    let mut response = swarm_api(format!("/users/self"), access_token)
        .await
        .with_context(|| format!("unable to retrieve information about the user"))?;
    let response = response
        .get_mut("user")
        .take()
        .ok_or_else(|| anyhow::anyhow!("unable to retrieve user info for swarm"))?
        .take();
    Ok(serde_json::from_value(response)?)
}

async fn get_swarm_callback(
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

    let access_token = swarm_get_access_token(
        &state.flags.swarm_client_id,
        &state.flags.swarm_client_secret,
        &format!("{}/swarm/callback", state.flags.base_url),
        code,
    )
    .await
    .from_err()?;
    tracing::debug!(?access_token, "swarm access token");

    let swarm_user = swarm_get_me(&access_token).await.from_err()?;
    tracing::debug!(?swarm_user, "swarm user");
    user.swarm_id = swarm_user.id.clone();
    user.swarm_access_token = access_token;
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

#[derive(Deserialize, Debug)]
struct SwarmLocation {
    country: Option<String>,
    city: Option<String>,
    state: Option<String>,
}

impl SwarmLocation {
    fn to_string(&self) -> Option<String> {
        match (
            self.city.as_ref(),
            self.state.as_ref(),
            self.country.as_ref(),
        ) {
            (Some(city), Some(state), _) => Some(format!("{}, {}", city, state)),
            (None, Some(state), Some(country)) => Some(format!("{}, {}", state, country)),
            (None, None, Some(country)) => Some(country.to_string()),
            (_, _, _) => None,
        }
    }
}

#[derive(Deserialize, Debug)]
struct SwarmVenue {
    id: String,
    name: String,
    location: SwarmLocation,
}

#[derive(Deserialize, Debug)]
struct SwarmCheckin {
    id: String,
    r#type: String,
    private: Option<bool>,
    shout: Option<String>,
    user: SwarmUser,
    venue: SwarmVenue,
    #[serde(default)]
    with: Vec<SwarmUser>,
}

#[derive(Deserialize, Debug)]
struct SwarmCheckinDetail {
    #[serde(flatten)]
    basic: SwarmCheckin,

    #[serde(rename = "checkinShortUrl")]
    checkin_short_url: String,
}

#[derive(Deserialize, Debug)]
struct SwarmPush {
    checkin: String,
    secret: String,
}

async fn get_checkin_details(access_token: &str, checkin_id: &str) -> Result<SwarmCheckinDetail> {
    let mut response = swarm_api(format!("/checkins/{}", checkin_id), access_token).await?;
    let response = response
        .get_mut("checkin")
        .take()
        .ok_or_else(|| anyhow::anyhow!("response from Swarm API does not contain checkin"))?
        .take();

    Ok(serde_json::from_value(response)?)
}

fn get_shout(checkin: &SwarmCheckin) -> Option<String> {
    let shout = checkin.shout.clone();
    if checkin.with.is_empty() {
        return shout;
    }

    let Some(shout) = shout else {
        return None;
    };

    // Attempt to check if the shout actually contains something
    let names = checkin
        .with
        .iter()
        .map(|user| user.first_name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let with_names = format!("with {}", names);

    let stripped = shout.trim_end_matches(&with_names).trim();
    if stripped.is_empty() {
        None
    } else {
        Some(shout)
    }
}

async fn post_swarm_push(
    State(state): State<Arc<AppState>>,
    Form(SwarmPush { checkin, secret }): Form<SwarmPush>,
) -> Result<(), String> {
    tracing::debug!(%checkin, "received push event");
    if secret != state.flags.swarm_push_secret {
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
    let Ok(Some(user_id)) = state.db.swarm_mapping.get(&checkin.user.id) else {
        tracing::warn!(
            user_id = checkin.user.id,
            "received push event for unknown user"
        );
        return Ok(());
    };
    let Ok(Some(user)) = state.db.get_user(String::from_utf8_lossy(&user_id)) else {
        tracing::warn!(
            user_id = checkin.user.id,
            "received push event for unknown user"
        );
        return Ok(());
    };
    let mastodon = user.get_mastodon();

    let country = checkin
        .venue
        .location
        .to_string()
        .map(|c| format!(" in {}", c))
        .unwrap_or_default();

    let details = match get_checkin_details(&user.swarm_access_token, &checkin.id).await {
        Ok(details) => details,
        Err(e) => {
            tracing::warn!(?checkin, ?e, "unable to retrieve checkin details");
            return Ok(());
        }
    };

    let url = details.checkin_short_url;
    let status = if let Some(shout) = get_shout(&checkin) {
        format!("{} (@ {}{}) {}", shout, checkin.venue.name, country, url)
    } else {
        tracing::info!("no shout for checkin {}, skip posting.", checkin.id);
        return Ok(());
    };

    tracing::debug!(checkin=%checkin.id, %status, "posting status");

    if let Err(e) = mastodon
        .new_status(NewStatus {
            status: Some(status),
            ..Default::default()
        })
        .await
    {
        tracing::warn!("unable to post status: {}", e);
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let flags = Flags::parse();
    let address = flags.address.clone();
    let database = flags.database.clone();

    let state = Arc::new(AppState {
        flags,
        db: model::Database::open(&database).unwrap(),
        signing_key: simple_cookie::generate_signing_key(),
    });

    let app = Router::new()
        .route("/", get(get_home).post(post_home))
        .route("/mastodon/callback", get(get_mastodon_callback))
        .route("/swarm", get(get_swarm))
        .route("/swarm/callback", get(get_swarm_callback))
        .route("/swarm/push", post(post_swarm_push))
        .with_state(state);

    tracing::info!("Going to listen at http://{}", address);

    axum::Server::bind(&address.parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

#[test]
fn test_get_shout() {
    let checkin: SwarmCheckin = serde_json::from_str(
        r#"{
  "id": "123",
  "createdAt": 1234,
  "type": "checkin",
  "visibility": "closeFriends",
  "shout": "with Alex, Bob",
  "timeZoneOffset": -480,
  "with": [
    {
      "id": "123",
      "firstName": "Alex",
      "lastName": "A",
      "handle": ""
    },
    {
      "id": "123",
      "firstName": "Bob",
      "lastName": "B",
      "handle": ""
    }
  ],
  "editableUntil": 1736735702000,
  "user": {
    "id": "123",
    "firstName": "Rice",
    "lastName": "R",
    "handle": "fanzeyi"
  },
  "venue": {
    "id": "123",
    "name": "A Place",
    "contact": {},
    "location": {
      "address": "123 A St",
      "lat": 1,
      "lng": -1,
      "postalCode": "10000",
      "cc": "US",
      "city": "New York",
      "state": "NY",
      "country": "United States"
    },
    "categories": [],
    "verified": false,
    "stats": { "tipCount": 0, "usersCount": 1, "checkinsCount": 1 },
    "allowMenuUrlEdit": true,
    "beenHere": { "lastCheckinExpiredAt": 0 },
    "createdAt": 1700966000
  }
}"#,
    )
    .unwrap();

    let shout = get_shout(&checkin);
    assert_eq!(shout, None);
}

#[test]
fn test_get_shout_with_content() {
    let checkin: SwarmCheckin = serde_json::from_str(
        r#"{
  "id": "123",
  "createdAt": 1234,
  "type": "checkin",
  "visibility": "closeFriends",
  "shout": "with this is a test with Alex, Bob",
  "timeZoneOffset": -480,
  "with": [
    {
      "id": "123",
      "firstName": "Alex",
      "lastName": "A",
      "handle": ""
    },
    {
      "id": "123",
      "firstName": "Bob",
      "lastName": "B",
      "handle": ""
    }
  ],
  "editableUntil": 1736735702000,
  "user": {
    "id": "123",
    "firstName": "Rice",
    "lastName": "R",
    "handle": "fanzeyi"
  },
  "venue": {
    "id": "123",
    "name": "A Place",
    "contact": {},
    "location": {
      "address": "123 A St",
      "lat": 1,
      "lng": -1,
      "postalCode": "10000",
      "cc": "US",
      "city": "New York",
      "state": "NY",
      "country": "United States"
    },
    "categories": [],
    "verified": false,
    "stats": { "tipCount": 0, "usersCount": 1, "checkinsCount": 1 },
    "allowMenuUrlEdit": true,
    "beenHere": { "lastCheckinExpiredAt": 0 },
    "createdAt": 1700966000
  }
}"#,
    )
    .unwrap();

    let shout = get_shout(&checkin);
    assert_eq!(
        shout,
        Some("with this is a test with Alex, Bob".to_string())
    );
}
