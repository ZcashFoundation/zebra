//! Types for unified addresses

/// `z_listunifiedreceivers` response
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Response {
    #[serde(skip_serializing_if = "String::is_empty")]
    orchard: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    sapling: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    p2pkh: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    p2sh: String,
}

impl Response {
    /// Create a new response for z_listunifiedreceivers given individual addresses.
    pub fn new(orchard: String, sapling: String, p2pkh: String, p2sh: String) -> Response {
        Response {
            orchard,
            sapling,
            p2pkh,
            p2sh,
        }
    }
}
