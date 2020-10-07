- Feature Name: Block subsidy
- Start Date: 2020-10-05
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/0000)
- Zebra Issue: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/issues/0000)

# Summary
[summary]: #summary

Block subsidy is the calculation and validation of rules defined in the Zcash protocol that apply to the coinbase transaction of all incoming blocks. The calculation of block subsidy on each block consists of amounts paid to miners and amounts paid to other participants(founders or funding stream receivers), it also specifies the receiver of these rewards.

At any time in the blockchain history specific rules apply to the coinbase transaction. This validation rules and calculations can change at Network Upgrades.

# Motivation
[motivation]: #motivation

All incoming blocks must be validated with the protocol rules, there is no way to avoid this logic to be present in any Zcash protocol compatible implementation such as Zebra.

The amount of rules, calculations, parameters, etc needed to implement the whole Block Subsidy can be overwhelming as each little piece comes together to make a whole. Zebra is in a position where we know all the rules up to the second halving so the implementation can and will be different from `zcashd` but at the end of the day all coinbase transactions will need to be validated as the protocol describes.

This document motivation is to have a clear roadmap about what is needed, the big picture of all components and how they interact together so we can separate a very big task into smaller pieces that we believe will make the implementation easier and better.

# Definitions
[definitions]: #definitions

- **founders reward**: The portion of the block reward that goes into a pre defined founder address. 
- **funding streams**:  The portion of the block reward that goes into a pre defined funding stream address.
- **miner subsidy**: The portion of the block reward that goes into the miner of the block. 
- **coinbase transaction**: The first transaction in a block where block subsidy is done.
- **NU**: Network Upgrade.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

In Zebra the consensus related code lives in the `zebra-consensus` crate. It is natural to add the block subsidy code here. The following module is suggested to be created by this design:

- `zebra-consensus/src/block/subsidy/subsidy.rs` 

Inside `zebra-consensus/src/block/subsidy/` the following submodules will be created:

- `general.rs`: General block reward functions and utilities.
- `founders_reward.rs`: Specific functions related to funders reward.
- `funding_streams.rs`: Specific functions for funding streams.

In addition to calculations the block subsidy requires constants defined in the protocol. The implementation will also create additional constants, all of them will live at:

- `zebra-consensus/src/parameters/subsidy.rs`

Checking functions for blocks are implemented at `zebra-consensus/src/block/check.rs` and they are called at `zebra-consensus/src/block.rs`. This follows the already existing structure for block validation in Zebra.

It is important to note that the Genesis block, BeforeOverwinter, and Overwinter blocks are verified by the CheckpointVerifier so they are not considered in this design. The following table will show what periods are included and what is not in this proposal:

| Height                                 | Miner    | Founder reward | Funding streams | Shielded Coinbase |
|----------------------------------------|:--------:|:--------------:|:---------------:|:------------------|             
| ~Genesis~                              | ~**0%**~ | ~**0%**~       | ~**0%**~        | ~**No**~          |
| ~Slow Start Shift..Slow Start Interval~| ~80%~    | ~20%~          | ~0%~            | ~No~              |
| ~Slow Start Interval..Overwinter~      | ~80%~    | ~20%~          | ~0%~            | ~No~              |
| Overwinter..Sapling                    | 80%      | 20%            | 0%              | No                |
| Sapling..Blossom                       | 80%      | 20%            | 0%              | No                |
| Blossom..Heartwood                     | 80%      | 20%            | 0%              | No                |
| Heartwood..Canopy                      | 80%      | 20%            | 0%              | Yes               |
| Canopy..Second Halving                 | 80%      | 0%             | 20%             | Yes               |
| ~Second Halving..~                     | ~100%~   | ~0%~           | ~0%~            | ~Yes~             |

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

Given the module structure proposed above all the block subsidy calculations from the protocol must be implemented. The final goal is to do checks for each incoming block and make sure they pass the consensus rules.

To do the calculations and checks the following constants and functions need to be introduced:

## Constants

- `SLOW_START_INTERVAL`
- `SLOW_START_SHIFT`
- `MAX_BLOCK_SUBSIDY`
- `PRE_BLOSSOM_POW_TARGET_SPACING`
- `POST_BLOSSOM_POW_TARGET_SPACING`
- `BLOSSOM_POW_TARGET_SPACING_RATIO`
- `PRE_BLOSSOM_HALVING_INTERVAL`
- `POST_BLOSSOM_HALVING_INTERVAL`
- `FOUNDERS_FRACTION_DIVISOR`
- `FOUNDER_ADDRESS_CHANGE_INTERVAL`
- `FOUNDERS_REWARD_ADDRESSES_MAINNET`
- `FOUNDERS_REWARD_ADDRESSES_TESTNET`

### Funding streams parameter constants

The design suggests to implement the parameters needed for funding streams as:

```
/// The funding stream receiver categories
pub enum FundingStreamsReceivers {
    /// Electric Coin Company
    ECC,
    /// ZCash Foundation
    ZF,
    /// Major Grants
    MG,
}

/// The numerator for each funding stream receiving category 
/// as described in [protocol specification §7.9.1][7.9.1].
/// 
/// [7.9.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
const FUNDINGSTREAMS_RECEIVERS_NUMERATORS: &[(i32, FundingStreamsReceivers)] = &[
    (7, FundingStreamsReceivers::ECC),
    (5, FundingStreamsReceivers::ZF),
    (8, FundingStreamsReceivers::MG),
];

/// Denominator as described in [protocol specification §7.9.1][7.9.1].
/// 
/// [7.9.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub const FUNDINGSTREAM_DENOMINATOR: i32 = 100;

/// Height intervals for funding streams on each network.
pub enum FundingStreamsHeightIntervals {
    /// For the Mainnet
    Mainnet,
    /// For the Testnet
    Testnet,
}

/// Start and end Heights for funding streams 
/// as described in [protocol specification §7.9.1][7.9.1].
/// 
/// [7.9.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
const FUNDINGSTREAMS_HEIGHT_INTERVALS: &[(Height, Height, FundingStreamsHeightIntervals)] = &[
    (Height(1_046_400), Height(2_726_400), FundingStreamsHeightIntervals::Mainnet),
    (Height(1_028_500), Height(2_796_000), FundingStreamsHeightIntervals::Testnet),
];
```

## General subsidy

The block subsidy and miner reward among other utility functions are inside the general subsidy category.

- `block_subsidy(Height, Network) -> Result<Amount<NonNegative>, Error>` - Total block subsidy.
- `miner_subsidy(Height, Network) -> Result<Amount<NonNegative>, Error>` - Miner portion.
- `transaction_fees(&Block) -> Result<Amount<NonNegative>, Error>` - Sum of all the transaction fees.
- `coinbase_sum_outputs(&Transaction) -> Result<Amount<NonNegative>, Error>` - Sum of all output values in the coinbase transaction.
- `find_output_with_amount(&Transaction, Amount<NonNegative>) -> Vec<transparent::Output>` - Outputs where value equal to Amount.

## Founders reward

Only functions specific to calculation of founders reward.

- `founders_reward(Height, Network) -> Result<Amount<NonNegative>, Error>` - Founders reward portion for this block or 0.
- `founders_reward_address(Height, Network) -> Result<String, Error>` - Address of the receiver founder at this block, address is a `String` because it can be a transparent or sapling address(After Heartwood/ZIP-213). 

## Funding streams

Only functions specific to the calculation of funding streams.

- `funding_stream(height, newtork) -> Result<Amount<NonNegative>, Error>` - Funding stream portion for this block or 0.
- `funding_stream_address(height, network) -> Result<String, Error>` - Address of the funding stream receiver at this block. The same as founders reward the returned address is a `String`.

## Consensus rules

All consensus rules for block subsidy are validated in a function inside `zebra-consensus/src/block/check.rs`:

`subsidy_is_correct(Network, &Block) -> Result<(), BlockError>`

The following consensus rules must be applied to `subsidy_is_correct()`:

### 1 - Founders reward:

*[Pre-Canopy] A coinbase transaction at `height` **MUST** include at least one output that pays exactly `FoundersReward(height)` zatoshi with a standard P2SH script of the form `OP_HASH160 FounderRedeemScriptHash (height) OP_EQUAL` as its scriptPubKey.* https://zips.z.cash/protocol/protocol.pdf#foundersreward

We make use of the founders reward functions here. We get the amount of the reward with `founders_reward(height, network)` and we get the addresses for the reward at height with `founders_reward_address(height, network)`. Next we cab get a list of outputs that match the amount with `find_output_with_amount()` and with it we check if it matches the address.

### 2 - Funding stream:

*[Canopy onward]  The coinbase transaction at `height` **MUST**  contain at least one output per funding stream `fs` active at
height, that pays `fs.Value(height)` zatoshi in the prescribed way to the stream’s recipient address represented by `fs.AddressList` of `fs.AddressIndex(height)`.* https://zips.z.cash/protocol/protocol.pdf#fundingstreams

We make use of the funding streams functions here, similar to founders reward . We get the amount using `funding_stream(height, network)` and then the address with `funding_stream_address(height, network)`. The validation is again the same as with founders reward, we need to look for an output that pays the amount to the address. The same utility function `find_output_with_amount()` can be used here. 

### 3 - Miner subsidy:

*The transaction fees are the sum of each transactions inputs, minus its outputs, for all non-coinbase transactions in a block the sum of the coinbase transaction outputs must be less than or equal to the block subsidy plus transaction fees (because miners can destroy coins)* https://github.com/ZcashFoundation/zebra/pull/1051#issuecomment-700325653

So the rule is the following before Canopy:

`coinbase_sum_outputs() <= miner_subsidy() + founders_reward() + transaction_fees()` 

and after Canopy:

`coinbase_sum_outputs() <= miner_subsidy() + funding_stream() + transaction_fees()`

### 4 - Shielded coinbase:

*Zip-213 - Shielded Coinbase* https://zips.z.cash/zip-0213#specification

Pending
