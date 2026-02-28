//! Mining config

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};

use strum_macros::EnumIter;
use zcash_address::ZcashAddress;
use zebra_chain::parameters::NetworkKind;

/// Mining configuration section.
#[serde_as]
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Address for receiving miner subsidy and tx fees.
    ///
    /// Used in coinbase tx constructed in `getblocktemplate` RPC.
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub miner_address: Option<ZcashAddress>,

    /// Optional data that Zebra will include in the transparent input of a coinbase transaction.
    /// Limited to 94 bytes.
    ///
    /// If this string is hex-encoded, it will be hex-decoded into bytes. Otherwise, it will be
    /// UTF-8 encoded into bytes.
    pub miner_data: Option<String>,

    /// Optional shielded memo that Zebra will include in the output of a shielded coinbase
    /// transaction. Limited to 512 bytes.
    ///
    /// Applies only if [`Self::miner_address`] contains a shielded component.
    pub miner_memo: Option<String>,

    /// Mine blocks using Zebra's internal miner, without an external mining pool or equihash solver.
    ///
    /// This experimental feature is only supported on regtest as it uses null solutions and skips checking
    /// for a valid Proof of Work.
    ///
    /// The internal miner is off by default.
    #[serde(default)]
    pub internal_miner: bool,
}

impl Config {
    /// Is the internal miner enabled using at least one thread?
    #[cfg(feature = "internal-miner")]
    pub fn is_internal_miner_enabled(&self) -> bool {
        // TODO: Changed to return always false so internal miner is never started. Part of https://github.com/ZcashFoundation/zebra/issues/8180
        // Find the removed code at https://github.com/ZcashFoundation/zebra/blob/v1.5.1/zebra-rpc/src/config/mining.rs#L83
        // Restore the code when conditions are met. https://github.com/ZcashFoundation/zebra/issues/8183
        self.internal_miner
    }
}

/// The desired address type for the `mining.miner_address` field in the config.
#[derive(EnumIter, Eq, PartialEq, Default, Hash)]
pub enum MinerAddressType {
    /// A unified address, containing the components of all the other address types.
    Unified,
    /// A Sapling address.
    Sapling,
    /// A transparent address.
    #[default]
    Transparent,
}

lazy_static::lazy_static! {
    /// Hard-coded addresses which can be used in the `mining.miner_address` field.
    ///
    /// All addresses come from a single address:
    ///
    /// - addresses for different networks are only different encodings of the same address;
    /// - addresses of different types are components of the same unified address.
    pub static ref MINER_ADDRESS: HashMap<NetworkKind, HashMap<MinerAddressType, &'static str>> = [
        (NetworkKind::Mainnet, [
            (MinerAddressType::Unified, "u1cymdny2u2vllkx7t5jnelp0kde0dgnwu0jzmggzguxvxj6fe7gpuqehywejndlrjwgk9snr6g69azs8jfet78s9zy60uepx6tltk7ee57jlax49dezkhkgvjy2puuue6dvaevt53nah7t2cc2k4p0h0jxmlu9sx58m2xdm5f9sy2n89jdf8llflvtml2ll43e334avu2fwytuna404a"),
            // (MinerAddressType::Orchard, "u1hmfjpqdxaec3mvqypl7fkqcy53u438csydljpuepsfs7jx6sjwyznuzlna8qsslj3tg6sn9ua4q653280aqv4m2fjd4csptwxq3fjpwy"),
            (MinerAddressType::Sapling, "zs1xl84ekz6stprmvrp39s77mf9t953nqjndwlcjtzfrr3cgjjez87639xm4u9pfuvylrhec3uryy5"),
            (MinerAddressType::Transparent, "t1T92bmyoPM7PTSWUnaWnLGcxkF6Jp1AwMY"),
        ].into()),
        (NetworkKind::Testnet, [
            (MinerAddressType::Unified, "utest10a8k6aw5w33kvyt7x6fryzu7vvsjru5vgcfnvr288qx2zm6p63ygcajtaze0px08t583dyrgr42vasazjhhnntus2tqrpkzu0dm2l4cgf3ld6wdqdrf3jv8mvfx9c80e73syer9l2wlgawjtf7yvj0eqwdf354trtelxnr0fhpw9792eaf49ghstkyftc9lwqqwy4ye0cleagp4nzyt"),
            // (MinerAddressType::Orchard, "utest10zg6frxk32ma8980kdv9473e4aclw7clq9hydzcj6l349pkqzxk2mmj3cn7j5x38w6l4wyryv50whnlrw0k9agzpdf5fxyj7kq96ukcp"),
            (MinerAddressType::Sapling, "ztestsapling1xl84ekz6stprmvrp39s77mf9t953nqjndwlcjtzfrr3cgjjez87639xm4u9pfuvylrhecet38rq"),
            (MinerAddressType::Transparent, "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"),
        ].into()),
        (NetworkKind::Regtest, [
            (MinerAddressType::Unified, "uregtest1efxggx6lduhm2fx5lnrhxv7h7kpztlpa3ahf3n4w0q0zj5epj4av9xjq6ljsja3xk8z7rzd067kc7mgpy9448rdfzpfjz5gq389zdmpgnk6rp4ykk0xk6cmqw6zqcrnmsuaxv3yzsvcwsd4gagtalh0uzrdvy03nhmltjz2eu0232qlcs0zvxuqyut73yucd9gy5jaudnyt7yqhgpqv"),
            // (MinerAddressType::Orchard, "uregtest1pszqlgxaf5w8mu2yd9uygg8cswp0ec4f7eejqnqc35tztw4tk0sxnt3pym2f3s2872cy2ruuc5n8y9cen5q6ngzlmzu8ztrjesv8zm9j"),
            (MinerAddressType::Sapling, "zregtestsapling1xl84ekz6stprmvrp39s77mf9t953nqjndwlcjtzfrr3cgjjez87639xm4u9pfuvylrhecx0c2j8"),
            (MinerAddressType::Transparent, "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"),
        ].into()),
    ].into();
}
