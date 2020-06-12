//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use zebra_network::Config as NetworkSection;

/// Zebrad Configuration
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct ZebradConfig {
    /// Tracing configuration
    pub tracing: TracingSection,
    /// Networking configuration
    pub network: NetworkSection,
    /// Metrics configuration
    pub metrics: MetricsSection,
}

/// Tracing configuration section.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TracingSection {
    /// The filter used for tracing events.
    pub filter: Option<String>,
}

impl TracingSection {
    pub fn populated() -> Self {
        Self {
            filter: Some("info".to_owned()),
        }
    }
}

/// Metrics configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    pub endpoint_addr: SocketAddr,
}

impl Default for MetricsSection {
    fn default() -> Self {
        Self {
            endpoint_addr: "127.0.0.1:9999".parse().unwrap(),
        }
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test_toml_ser() -> color_eyre::Result<()> {
        let default_config = super::ZebradConfig::default();
        println!("Default config: {:?}", default_config);

        println!("Toml:\n{}", toml::Value::try_from(&default_config)?);

        Ok(())
    }
}
