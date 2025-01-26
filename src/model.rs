use std::collections::HashMap;
use std::path::Path;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use arbitrary::Arbitrary;
use arbitrary::Unstructured;
use mastodon_async::registration::Registered;
use mastodon_async::Data;
use mastodon_async::Mastodon;
use mastodon_async::NewStatus;
use serde::Deserialize;
use serde::Serialize;

use crate::swarm::SwarmCheckin;
use crate::swarm::SwarmUserApi;

#[derive(Clone)]
pub struct Database {
    #[allow(dead_code)]
    db: sled::Db,
    pub registration: sled::Tree,
    pub user: sled::Tree,
    pub swarm_mapping: sled::Tree,
}

impl Database {
    pub fn open<P: AsRef<Path>>(p: P) -> Result<Self> {
        let db = sled::open(p)?;
        let registration = db.open_tree("registration")?;
        let user = db.open_tree("user")?;
        let swarm_mapping = db.open_tree("swarm_mapping")?;
        Ok(Self {
            db,
            registration,
            user,
            swarm_mapping,
        })
    }

    pub fn get_registration(&self, instance_url: &str) -> Result<Option<AppRegistration>> {
        if let Some(registration) = self.registration.get(instance_url)? {
            Ok(Some(bincode::deserialize(&registration)?))
        } else {
            Ok(None)
        }
    }

    pub fn save_registration(&self, key: String, registered: Registered) -> Result<()> {
        self.registration
            .insert(key, bincode::serialize(&AppRegistration::from(registered))?)?;
        Ok(())
    }

    pub fn get_user<T: AsRef<str>>(&self, key: T) -> Result<Option<User>> {
        if let Some(user) = self.user.get(key.as_ref())? {
            Ok(Some(bincode::deserialize(&user)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_mastodon_user(&self, instance_url: &str, mastodon_id: &str) -> Result<Option<User>> {
        self.get_user(format!("{}:{}", instance_url, mastodon_id))
    }

    pub fn create_user(&self, instance_url: &str, mastodon_id: &str, data: Data) -> Result<User> {
        let user = User {
            mastodon: data,
            swarm_id: "".to_string(),
            swarm_access_token: "".to_string(),
        };
        self.user.insert(
            format!("{}:{}", instance_url, mastodon_id),
            bincode::serialize(&user)?,
        )?;
        Ok(user)
    }

    pub fn get_users(&self) -> Result<HashMap<String, User>> {
        self.user
            .iter()
            .map(|x| {
                let x = x?;
                Ok((
                    String::from_utf8(x.0.to_vec())?,
                    bincode::deserialize(&x.1)?,
                ))
            })
            .collect()
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct AppRegistration {
    pub base: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub scopes: Vec<u8>,
}

impl AppRegistration {
    pub fn into_registered(self) -> Result<Registered> {
        Ok(Registered::from_parts(
            &self.base,
            &self.client_id,
            &self.client_secret,
            &self.redirect_uri,
            bincode::deserialize(&self.scopes)
                .with_context(|| anyhow!("unable to deserialize scope '{:?}'", self.scopes))?,
            false,
        ))
    }
}

impl From<Registered> for AppRegistration {
    fn from(registered: Registered) -> Self {
        let (base, client_id, client_secret, redirect_uri, scopes, _) = registered.into_parts();

        Self {
            base,
            client_id,
            client_secret,
            redirect_uri,
            scopes: bincode::serialize(&scopes).unwrap(),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Arbitrary)]
pub struct User {
    #[arbitrary(with = arbitrary_mastodon_data)]
    pub mastodon: Data,
    pub swarm_id: String,
    pub swarm_access_token: String,
}

fn arbitrary_mastodon_data(u: &mut Unstructured) -> arbitrary::Result<Data> {
    Ok(Data {
        base: u.arbitrary::<String>()?.into(),
        client_id: u.arbitrary::<String>()?.into(),
        client_secret: u.arbitrary::<String>()?.into(),
        redirect: u.arbitrary::<String>()?.into(),
        token: u.arbitrary::<String>()?.into(),
    })
}

impl User {
    pub fn get_mastodon(&self) -> Mastodon {
        self.mastodon.clone().into()
    }

    pub fn get_swarm(&self) -> SwarmUserApi {
        SwarmUserApi::new(self.swarm_access_token.clone())
    }

    pub async fn post_checkin(
        &self,
        checkin: &SwarmCheckin,
        friends_map: &HashMap<String, String>,
    ) -> Result<()> {
        let mastodon = self.get_mastodon();
        let swarm = self.get_swarm();

        let country = checkin
            .venue
            .location
            .to_string()
            .map(|c| format!(" in {}", c))
            .unwrap_or_default();

        let details = match swarm.get_checkin_details(&checkin.id).await {
            Ok(details) => details,
            Err(e) => {
                tracing::warn!(?checkin, ?e, "unable to retrieve checkin details");
                return Ok(());
            }
        };

        let url = details.checkin_short_url;
        let status = if let Some(shout) = crate::swarm::get_shout(&checkin, &friends_map) {
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
}

#[test]
fn test_get_users() {
    arbtest::arbtest(|u| {
        let id1 = "https://example.com:1";
        let id2 = "https://example.com:2";
        let id3 = "https://example.com:3";
        let user1: User = u.arbitrary()?;
        let user2: User = u.arbitrary()?;
        let user3: User = u.arbitrary()?;
        let db = Database::open("test.db").unwrap();
        db.user.clear().unwrap();
        db.user
            .insert(id1, bincode::serialize(&user1).unwrap())
            .unwrap();
        db.user
            .insert(id2, bincode::serialize(&user2).unwrap())
            .unwrap();
        db.user
            .insert(id3, bincode::serialize(&user3).unwrap())
            .unwrap();

        let users = db.get_users().unwrap();
        assert_eq!(users.len(), 3);
        assert_eq!(users[id1].mastodon, user1.mastodon);
        assert_eq!(users[id1].swarm_id, user1.swarm_id);
        assert_eq!(users[id1].swarm_access_token, user1.swarm_access_token);
        assert_eq!(users[id2].mastodon, user2.mastodon);
        assert_eq!(users[id2].swarm_id, user2.swarm_id);
        assert_eq!(users[id2].swarm_access_token, user2.swarm_access_token);
        assert_eq!(users[id3].mastodon, user3.mastodon);
        assert_eq!(users[id3].swarm_id, user3.swarm_id);
        assert_eq!(users[id3].swarm_access_token, user3.swarm_access_token);
        Ok(())
    });
}
