use std::path::Path;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use mastodon_async::entities::instance;
use mastodon_async::registration::Registered;
use mastodon_async::Data;
use mastodon_async::Mastodon;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

pub struct Database {
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

#[derive(Deserialize, Serialize, Debug)]
pub struct User {
    pub mastodon: Data,
    pub swarm_id: String,
    pub swarm_access_token: String,
}

impl User {
    pub fn get_mastodon(&self) -> Mastodon {
        self.mastodon.clone().into()
    }
}
