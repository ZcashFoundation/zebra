//! Types for the `getblocksubsidy` RPC.

use zebra_chain::{
    amount::{Amount, NonNegative},
    parameters::subsidy::FundingStreamReceiver,
    transparent,
};
use zebra_consensus::funding_stream_recipient_info;

use crate::methods::get_block_template_rpcs::types::zec::Zec;

/// A response to a `getblocksubsidy` RPC request
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockSubsidy {
    /// An array of funding stream descriptions.
    /// Always present befoe NU6, Zebra returns an error for heights before the first halving.
    #[serde(rename = "fundingstreams", skip_serializing_if = "Vec::is_empty")]
    pub funding_streams: Vec<FundingStream>,

    /// An array of lockbox stream descriptions.
    /// Always present after NU6.
    #[serde(rename = "lockboxstreams", skip_serializing_if = "Vec::is_empty")]
    pub lockbox_streams: Vec<FundingStream>,

    /// The mining reward amount in ZEC.
    ///
    /// This does not include the miner fee.
    pub miner: Zec<NonNegative>,

    /// The founders' reward amount in ZEC.
    ///
    /// Zebra returns an error when asked for founders reward heights,
    /// because it checkpoints those blocks instead.
    pub founders: Zec<NonNegative>,

    /// The total funding stream amount in ZEC.
    #[serde(rename = "fundingstreamstotal")]
    pub funding_streams_total: Zec<NonNegative>,

    /// The total lockbox stream amount in ZEC.
    #[serde(rename = "lockboxtotal")]
    pub lockbox_total: Zec<NonNegative>,

    /// The total block subsidy amount in ZEC.
    ///
    /// This does not include the miner fee.
    #[serde(rename = "totalblocksubsidy")]
    pub total_block_subsidy: Zec<NonNegative>,
}

impl Default for BlockSubsidy {
    fn default() -> Self {
        Self {
            funding_streams: vec![],
            lockbox_streams: vec![],
            miner: Zec::from_lossy_zec(0.0).unwrap(),
            founders: Zec::from_lossy_zec(0.0).unwrap(),
            funding_streams_total: Zec::from_lossy_zec(0.0).unwrap(),
            lockbox_total: Zec::from_lossy_zec(0.0).unwrap(),
            total_block_subsidy: Zec::from_lossy_zec(0.0).unwrap(),
        }
    }
}

/// A single funding stream's information in a  `getblocksubsidy` RPC request
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FundingStream {
    /// A description of the funding stream recipient.
    pub recipient: String,

    /// A URL for the specification of this funding stream.
    pub specification: String,

    /// The funding stream amount in ZEC.
    pub value: Zec<NonNegative>,

    /// The funding stream amount in zatoshis.
    #[serde(rename = "valueZat")]
    pub value_zat: Amount<NonNegative>,

    /// The transparent or Sapling address of the funding stream recipient.
    ///
    /// The current Zcash funding streams only use transparent addresses,
    /// so Zebra doesn't support Sapling addresses in this RPC.
    ///
    /// This is optional so we can support funding streams with no addresses (lockbox streams).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<transparent::Address>,
}

impl FundingStream {
    /// Convert a `receiver`, `value`, and `address` into a `FundingStream` response.
    pub fn new(
        receiver: FundingStreamReceiver,
        value: Amount<NonNegative>,
        address: Option<&transparent::Address>,
    ) -> FundingStream {
        let (recipient, specification) = funding_stream_recipient_info(receiver);

        FundingStream {
            recipient: recipient.to_string(),
            specification: specification.to_string(),
            value: value.into(),
            value_zat: value,
            address: address.cloned(),
        }
    }
}
