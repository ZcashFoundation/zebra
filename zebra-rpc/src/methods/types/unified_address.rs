//! Types for unified addresses

/// `z_listunifiedreceivers` response
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

impl Response {
    /// Create a new response for z_listunifiedreceivers given individual addresses.
    pub fn new(orchard: String, sapling: String, p2pkh: String, p2sh: String) -> Response {
        Response {
            orchard: if orchard.is_empty() {
                None
            } else {
                Some(orchard)
            },
            sapling: if sapling.is_empty() {
                None
            } else {
                Some(sapling)
            },
            p2pkh: if p2pkh.is_empty() { None } else { Some(p2pkh) },
            p2sh: if p2sh.is_empty() { None } else { Some(p2sh) },
        }
    }

    #[cfg(test)]
    /// Return the orchard payment address from a response, if any.
    pub fn orchard(&self) -> Option<String> {
        self.orchard.clone()
    }

    #[cfg(test)]
    /// Return the sapling payment address from a response, if any.
    pub fn sapling(&self) -> Option<String> {
        self.sapling.clone()
    }

    #[cfg(test)]
    /// Return the p2pkh payment address from a response, if any.
    pub fn p2pkh(&self) -> Option<String> {
        self.p2pkh.clone()
    }

    #[cfg(test)]
    /// Return the p2sh payment address from a response, if any.
    pub fn p2sh(&self) -> Option<String> {
        self.p2sh.clone()
    }
}
