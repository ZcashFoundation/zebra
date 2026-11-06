//! The network upgrade active at the chain tip, and the next scheduled upgrade.

use crate::rpc::RpcBlockchainInfo;

/// The target time between blocks after the Blossom upgrade, in seconds.
pub const TARGET_BLOCK_SPACING_SECONDS: u64 = 75;

/// A network upgrade that activates above the current chain tip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingUpgrade {
    pub name: String,
    pub activation_height: u32,
}

impl PendingUpgrade {
    /// Returns the number of blocks after `tip_height` until the upgrade activates.
    pub fn blocks_remaining(&self, tip_height: u32) -> u32 {
        self.activation_height.saturating_sub(tip_height)
    }

    /// Returns the expected seconds until activation, if blocks arrive at the target spacing.
    pub fn estimated_seconds_remaining(&self, tip_height: u32) -> u64 {
        u64::from(self.blocks_remaining(tip_height)) * TARGET_BLOCK_SPACING_SECONDS
    }
}

/// The upgrade that governs the chain tip, and the next one that is scheduled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpgradeSchedule {
    pub current: Option<String>,
    pub next: Option<PendingUpgrade>,
}

/// Reads the upgrade schedule from a `getblockchaininfo` response.
///
/// The next upgrade is the one with the lowest activation height above the chain tip.
pub fn upgrade_schedule(info: &RpcBlockchainInfo) -> UpgradeSchedule {
    let current = info
        .consensus
        .as_ref()
        .and_then(|consensus| info.upgrades.get(&consensus.chaintip))
        .map(|upgrade| upgrade.name.clone());
    let next = info
        .upgrades
        .values()
        .filter(|upgrade| upgrade.activationheight > info.blocks)
        .min_by_key(|upgrade| upgrade.activationheight)
        .map(|upgrade| PendingUpgrade {
            name: upgrade.name.clone(),
            activation_height: upgrade.activationheight,
        });
    UpgradeSchedule { current, next }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(tip: u32) -> RpcBlockchainInfo {
        serde_json::from_value(serde_json::json!({
            "blocks": tip,
            "bestblockhash": "00",
            "upgrades": {
                "37a5165b": {"name": "NU6.3", "activationheight": 3428143, "status": "active"},
                "aaaaaaaa": {"name": "NU8", "activationheight": 3600000, "status": "pending"},
                "77190ad9": {"name": "NU7", "activationheight": 3543405, "status": "pending"}
            },
            "consensus": {"chaintip": "37a5165b", "nextblock": "37a5165b"}
        }))
        .expect("test vector is valid getblockchaininfo JSON")
    }

    #[test]
    fn next_upgrade_is_the_lowest_activation_above_the_tip() {
        let schedule = upgrade_schedule(&info(3_543_365));
        assert_eq!(schedule.current.as_deref(), Some("NU6.3"));
        let next = schedule.next.expect("NU7 and NU8 are above the tip");
        assert_eq!(next.name, "NU7");
        assert_eq!(next.blocks_remaining(3_543_365), 40);
        assert_eq!(next.estimated_seconds_remaining(3_543_365), 3_000);
    }

    #[test]
    fn activated_upgrades_are_not_pending() {
        let schedule = upgrade_schedule(&info(3_543_405));
        assert_eq!(
            schedule.next.map(|upgrade| upgrade.name).as_deref(),
            Some("NU8")
        );
    }

    #[test]
    fn no_pending_upgrade_after_the_last_activation() {
        assert_eq!(upgrade_schedule(&info(4_000_000)).next, None);
    }

    #[test]
    fn missing_upgrade_fields_give_an_empty_schedule() {
        let info: RpcBlockchainInfo =
            serde_json::from_value(serde_json::json!({"blocks": 1, "bestblockhash": "00"}))
                .expect("required fields are present");
        assert_eq!(upgrade_schedule(&info), UpgradeSchedule::default());
    }
}
