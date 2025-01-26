use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone)]
pub struct SwarmApi {
    client_id: String,
    client_secret: String,
    callback_url: Url,
}

impl SwarmApi {
    pub fn new(client_id: String, client_secret: String, callback_url: Url) -> Self {
        Self {
            client_id,
            client_secret,
            callback_url,
        }
    }

    pub fn get_authenticate_url(&self) -> Url {
        Url::parse_with_params(
            "https://foursquare.com/oauth2/authenticate",
            &[
                ("client_id", self.client_id.as_str()),
                ("response_type", "code"),
                ("redirect_uri", self.callback_url.as_str()),
            ],
        )
        .expect("invalid swarm url")
    }

    pub async fn get_access_token(&self, code: &str) -> Result<SwarmUserApi> {
        let access_token_url = Url::parse_with_params(
            "https://foursquare.com/oauth2/access_token",
            &[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("grant_type", "authorization_code"),
                ("redirect_uri", self.callback_url.as_str()),
                ("code", code),
            ],
        )
        .expect("invalid swarm url");

        let response = reqwest::get(access_token_url).await?;
        let response = response.json::<serde_json::Value>().await?;
        let access_token = response
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("unable to retrieve access token for swarm"))?;

        Ok(SwarmUserApi::new(access_token.to_string()))
    }
}

pub struct SwarmUserApi {
    pub access_token: String,
}

impl SwarmUserApi {
    pub fn new(access_token: String) -> Self {
        Self { access_token }
    }

    async fn swarm_api(&self, method: String) -> Result<serde_json::Value> {
        let url = format!(
            "https://api.foursquare.com/v2{}?v=20220722&oauth_token={}",
            method, self.access_token
        );

        let response = reqwest::get(url).await?;
        let mut response = response.json::<serde_json::Value>().await?;
        let Some(response) = response.get_mut("response").map(|v| v.take()) else {
            return Err(anyhow::anyhow!("unable to retrieve response for swarm"));
        };
        Ok(response)
    }

    pub async fn get_me(&self) -> Result<SwarmUser> {
        let mut response = self
            .swarm_api(format!("/users/self"))
            .await
            .with_context(|| format!("unable to retrieve information about the user"))?;
        let response = response
            .get_mut("user")
            .take()
            .ok_or_else(|| anyhow::anyhow!("unable to retrieve user info for swarm"))?
            .take();
        Ok(serde_json::from_value(response)?)
    }

    pub async fn get_checkins(&self) -> Result<Vec<SwarmCheckin>> {
        let mut response = self
            .swarm_api(format!("/users/self/checkins"))
            .await
            .with_context(|| format!("unable to retrieve checkins for the user"))?;
        let response = response
            .get_mut("checkins")
            .take()
            .ok_or_else(|| anyhow::anyhow!("unable to retrieve checkins for the user"))?
            .take()
            .get_mut("items")
            .ok_or_else(|| anyhow::anyhow!("unable to retrieve checkins for the user"))?
            .take();

        Ok(serde_json::from_value(response)?)
    }

    pub async fn get_checkin_details(&self, checkin_id: &str) -> Result<SwarmCheckinDetail> {
        let mut response = self.swarm_api(format!("/checkins/{}", checkin_id)).await?;
        let response = response
            .get_mut("checkin")
            .take()
            .ok_or_else(|| anyhow::anyhow!("response from Swarm API does not contain checkin"))?
            .take();

        Ok(serde_json::from_value(response)?)
    }

    pub async fn get_latest_checkins(&self) -> Result<Vec<SwarmCheckin>> {
        let checkins = self.get_checkins().await?;
        Ok(checkins
            .into_iter()
            .filter(|c| !c.private.unwrap_or_default())
            .collect())
    }

    pub async fn get_last_checkin(&self, swarm_id: &str) -> Result<String> {
        Ok(self
            .get_latest_checkins()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no checkins found for user {}", swarm_id))?
            .id)
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SwarmUser {
    pub id: String,
    pub first_name: String,
    pub last_name: String,
    pub handle: String,
}

#[derive(Deserialize, Debug)]
pub struct SwarmLocation {
    country: Option<String>,
    city: Option<String>,
    state: Option<String>,
}

impl SwarmLocation {
    pub fn to_string(&self) -> Option<String> {
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
pub struct SwarmVenue {
    pub id: String,
    pub name: String,
    pub location: SwarmLocation,
}

#[derive(Deserialize, Debug)]
pub struct SwarmCheckin {
    pub id: String,
    pub r#type: String,
    pub private: Option<bool>,
    pub shout: Option<String>,
    pub user: Option<SwarmUser>,
    pub venue: SwarmVenue,
    #[serde(default)]
    pub with: Vec<SwarmUser>,
}

#[derive(Deserialize, Debug)]
pub struct SwarmCheckinDetail {
    #[serde(flatten)]
    pub basic: SwarmCheckin,

    #[serde(rename = "checkinShortUrl")]
    pub checkin_short_url: String,
}

#[derive(Deserialize, Debug)]
pub struct SwarmPush {
    pub checkin: String,
    pub secret: String,
}

pub fn get_shout(checkin: &SwarmCheckin, friends_map: &HashMap<String, String>) -> Option<String> {
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
        let names = checkin
            .with
            .iter()
            .map(|user| {
                if let Some(mastodon_id) = friends_map.get(&user.handle) {
                    format!("@{}", mastodon_id)
                } else {
                    user.first_name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");

        Some(format!("{} with {}", stripped, names))
    }
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

    let shout = get_shout(&checkin, &HashMap::new());
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
      "handle": "alex"
    },
    {
      "id": "123",
      "firstName": "Bob",
      "lastName": "B",
      "handle": "bob"
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

    let shout = get_shout(&checkin, &HashMap::new());
    assert_eq!(
        shout,
        Some("with this is a test with Alex, Bob".to_string())
    );
    let mut friends_map = HashMap::new();
    friends_map.insert("alex".to_string(), "alex@example.com".to_string());
    let shout = get_shout(&checkin, &friends_map);
    assert_eq!(
        shout,
        Some("with this is a test with @alex@example.com, Bob".to_string())
    );
}
