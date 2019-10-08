/// Configuration for networking code.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The user-agent to advertise.
    pub user_agent: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            user_agent: crate::constants::USER_AGENT.to_owned(),
        }
    }
}
