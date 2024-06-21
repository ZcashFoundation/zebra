//! Types for unified addresses

/// `z_listunifiedreceivers` response
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

impl Default for Response {
    fn default() -> Self {
        Self {
            orchard: "orchard address if any".to_string(),
            sapling: "sapling address if any".to_string(),
            p2pkh: "p2pkh address if any".to_string(),
            p2sh: "p2sh address if any".to_string(),
        }
    }
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

    #[cfg(test)]
    /// Return the orchard payment address from a response, if any.
    pub fn orchard(&self) -> Option<String> {
        match self.orchard.is_empty() {
            true => None,
            false => Some(self.orchard.clone()),
        }
    }

    #[cfg(test)]
    /// Return the sapling payment address from a response, if any.
    pub fn sapling(&self) -> Option<String> {
        match self.sapling.is_empty() {
            true => None,
            false => Some(self.sapling.clone()),
        }
    }

    #[cfg(test)]
    /// Return the p2pkh payment address from a response, if any.
    pub fn p2pkh(&self) -> Option<String> {
        match self.p2pkh.is_empty() {
            true => None,
            false => Some(self.p2pkh.clone()),
        }
    }

    #[cfg(test)]
    /// Return the p2sh payment address from a response, if any.
    pub fn p2sh(&self) -> Option<String> {
        match self.p2sh.is_empty() {
            true => None,
            false => Some(self.p2sh.clone()),
        }
    }
}
