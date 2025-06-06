//! Types for unified addresses

use derive_getters::Getters;
use derive_new::new;

/// `z_listunifiedreceivers` response
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    orchard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sapling: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    p2pkh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    p2sh: Option<String>,
}

impl Default for Response {
    fn default() -> Self {
        Self {
            orchard: Some("orchard address if any".to_string()),
            sapling: Some("sapling address if any".to_string()),
            p2pkh: Some("p2pkh address if any".to_string()),
            p2sh: Some("p2sh address if any".to_string()),
        }
    }
}
