use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SwarmUser {
    pub id: String,
    pub first_name: String,
    pub last_name: String,
    pub handle: String,
}

pub async fn swarm_api(method: String, access_token: &str) -> Result<serde_json::Value> {
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

pub async fn swarm_get_me(access_token: &str) -> Result<SwarmUser> {
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
    pub user: SwarmUser,
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

pub async fn get_checkin_details(
    access_token: &str,
    checkin_id: &str,
) -> Result<SwarmCheckinDetail> {
    let mut response = swarm_api(format!("/checkins/{}", checkin_id), access_token).await?;
    let response = response
        .get_mut("checkin")
        .take()
        .ok_or_else(|| anyhow::anyhow!("response from Swarm API does not contain checkin"))?
        .take();

    Ok(serde_json::from_value(response)?)
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
