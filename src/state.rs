use std::collections::HashMap;

pub struct AppState {
    pub flags: crate::Flags,
    pub db: crate::model::Database,
    pub signing_key: [u8; 32],
    pub friends_map: HashMap<String, String>,
}
