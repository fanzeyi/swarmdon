use std::{path::PathBuf, sync::Arc};

use axum::routing::post;
use axum::{routing::get, Router};
use clap::Parser;
use mastodon_async::scopes::Read;
use mastodon_async::{
    apps::{App, AppBuilder},
    scopes::{Scopes, Write},
};
use once_cell::sync::OnceCell;
use state::AppState;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

mod model;
mod routes;
mod state;
mod swarm;
mod utils;

#[derive(Debug, Parser)]
pub struct Flags {
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

    #[clap(long)]
    friends_map: Option<PathBuf>,
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

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let flags = Flags::parse();
    let address = flags.address.clone();
    let state = Arc::new(AppState::from_flags(flags));

    let app = Router::new()
        .route("/", get(routes::get_home).post(routes::post_home))
        .route("/mastodon/callback", get(routes::get_mastodon_callback))
        .route("/swarm", get(routes::get_swarm))
        .route("/swarm/callback", get(routes::get_swarm_callback))
        .route("/swarm/push", post(routes::post_swarm_push))
        .with_state(state);

    tracing::info!("Going to listen at http://{}", address);

    axum::Server::bind(&address.parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
