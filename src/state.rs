use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use std::sync::Arc;
use std::{collections::HashMap, path::Path};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::model::Database;
use crate::Flags;

pub struct AppState {
    pub flags: crate::Flags,
    pub db: crate::model::Database,
    pub signing_key: [u8; 32],
    pub friends_map: HashMap<String, String>,
    pub last_checkin: Option<Mutex<HashMap<String, String>>>,
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
    async fn fetch_last_checkin(db: &Database) -> Result<HashMap<String, String>> {
        let users = db
            .get_users()
            .context("failed to get all users from sled")?;

        users
            .into_iter()
            .map(|(id, user)| async move {
                let last_checkin = user.get_last_checkin().await?;
                Ok((id, last_checkin))
            })
            .collect::<JoinSet<_>>()
            .join_all()
            .await
            .into_iter()
            .collect()
    }

    pub async fn from_flags(flags: Flags) -> Self {
        let database = flags.database.clone();
        let db = Database::open(&database).unwrap();
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
        let last_checkin = if flags.with_polling {
            Some(Mutex::new(
                Self::fetch_last_checkin(&db)
                    .await
                    .context("unable to fetch last checkin")
                    .unwrap(),
            ))
        } else {
            None
        };

        tracing::debug!(?last_checkin, "last checkin");

        AppState {
            flags,
            db,
            signing_key: simple_cookie::generate_signing_key(),
            friends_map,
            last_checkin,
        }
    }

    pub async fn update_last_checkin(&self, user_id: &str, checkin_id: &str) {
        if let Some(last_checkin) = self.last_checkin.as_ref() {
            let mut last_checkin = last_checkin.lock().await;
            last_checkin.insert(user_id.to_string(), checkin_id.to_string());
        }
    }

    pub fn start_polling_task(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let state = self.clone();

        if state.last_checkin.is_none() {
            return tokio::spawn(async {});
        }

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                tracing::debug!("polling for latest checkins");
                let checkins = {
                    state
                        .last_checkin
                        .as_ref()
                        .unwrap()
                        .lock()
                        .await
                        .iter()
                        .map(|(id, last_checkin)| {
                            let id = id.clone();
                            let last_checkin = last_checkin.clone();
                            let db = state.db.clone();
                            async move {
                                let user = db
                                    .get_user(&id)
                                    .context("unable to get user")?
                                    .ok_or_else(|| anyhow!("user not found"))?;
                                let checkins = user
                                    .get_latest_checkins()
                                    .await
                                    .context("unable to get latest checkins")?;

                                Ok((
                                    id,
                                    checkins
                                        .into_iter()
                                        .take_while(|c| c.id != last_checkin)
                                        .collect::<Vec<_>>(),
                                ))
                            }
                        })
                }
                .collect::<JoinSet<_>>()
                .join_all()
                .await
                .into_iter()
                .collect::<Result<HashMap<_, _>>>();

                // checkins
                let checkins = match checkins {
                    Ok(checkins) => checkins,
                    Err(e) => {
                        tracing::error!(?e, "unable to get checkins");
                        continue;
                    }
                };

                for (id, mut checkins) in checkins.into_iter() {
                    if checkins.is_empty() {
                        continue;
                    }
                    let user = match state.db.get_user(&id).context("unable to get user") {
                        Ok(Some(user)) => user,
                        Ok(None) => {
                            tracing::error!(?id, "user not found");
                            continue;
                        }
                        Err(e) => {
                            tracing::error!(?e, id=?id, "unable to get user");
                            continue;
                        }
                    };

                    // ensures order of the checkins
                    checkins.reverse();

                    tracing::debug!(?checkins, "found missing checkins");
                    for checkin in &checkins {
                        if let Err(e) = user
                            .post_checkin(&checkin, &state.friends_map)
                            .await
                            .context("unable to post checkin")
                        {
                            tracing::error!(?e, checkin=%checkin.id, user=%id, "unable to post checkin");
                        }
                    }

                    state
                        .update_last_checkin(&id, &checkins.last().unwrap().id)
                        .await;
                }
            }
        })
    }
}
