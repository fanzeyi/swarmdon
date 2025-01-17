use anyhow::Context;
use anyhow::Result;
use std::{collections::HashMap, path::Path};

use crate::Flags;

pub struct AppState {
    pub flags: crate::Flags,
    pub db: crate::model::Database,
    pub signing_key: [u8; 32],
    pub friends_map: HashMap<String, String>,
}

fn read_friends_map(path: &Path) -> Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(path).context("unable to read friends map")?;
    let mut map = HashMap::new();
    for line in content.lines() {
        let (swarm_id, mastodon_id) = line.split_once('=').context("invalid line")?;
        map.insert(swarm_id.to_string(), mastodon_id.to_string());
    }
    Ok(map)
}

impl AppState {
    pub fn from_flags(flags: Flags) -> Self {
        let database = flags.database.clone();
        let friends_map = if let Some(friends_map) = flags.friends_map.as_ref() {
            match read_friends_map(friends_map) {
                Ok(map) => map,
                Err(e) => {
                    tracing::error!(?e, "unable to read friends map");
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };

        AppState {
            flags,
            db: crate::model::Database::open(&database).unwrap(),
            signing_key: simple_cookie::generate_signing_key(),
            friends_map,
        }
    }
}
