//! Merges the canonical output order with the exact-H UTXO set into survivor bits.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use color_eyre::eyre::{bail, ensure, Result};
use sha2::{Digest, Sha256};
use zebra_chain::{
    block::{self, Block, Height},
    parameters::{
        spentness_hints::{ArtifactEncoder, ParsedArtifact},
        Network,
    },
    transparent::{self, OrderedUtxo},
};
use zebra_state::{OutputLocation, ZebraDb};

use super::{exact_boundary, for_each_block, open, write};

/// An untrusted artifact, and the values a reviewer checks before pinning it.
pub(super) struct Generated {
    /// The complete artifact.
    pub(super) bytes: Vec<u8>,
    /// The terminal height H.
    pub(super) height: Height,
    /// The hash of the block at H.
    pub(super) hash: block::Hash,
    /// The number of outputs, which is the number of bits.
    pub(super) output_count: u64,
    /// The number of set bits.
    pub(super) survivors: u64,
    /// The SHA-256 of `bytes`: the pin's `spentness_hash`.
    pub(super) sha256: [u8; 32],
}

/// The survivor bits, packed while the canonical outputs are counted.
///
/// [`ArtifactEncoder`] needs the output count before the first bit, and the count is
/// only known after the pass over every block.
#[derive(Default)]
struct Bits {
    words: Vec<u64>,
    len: u64,
}

impl Bits {
    fn push(&mut self, bit: bool) {
        // The remainder is below 64.
        let offset = (self.len % 64) as u32;
        if offset == 0 {
            self.words.push(0);
        }
        if bit {
            *self
                .words
                .last_mut()
                .expect("a word was pushed for the first bit at each offset 0") |= 1 << offset;
        }
        self.len += 1;
    }

    fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        (0..self.len).map(|index| {
            // The quotient indexes `words`, which holds `len` bits, so it fits in usize.
            let word = self.words[(index / 64) as usize];
            word & (1 << (index % 64)) != 0
        })
    }
}

/// Generates the artifact for an archive whose tip is exactly `height`/`hash`.
///
/// Every UTXO must match exactly one canonical output, with an identical entry. Moving
/// H regenerates every bit, including the bits of older outputs.
pub(super) fn generate(
    db: &ZebraDb,
    network: &Network,
    height: Height,
    hash: block::Hash,
) -> Result<Generated> {
    let genesis = exact_boundary(db, network, height, hash)?;
    let mut utxos = db.utxos_by_location().peekable();
    let mut bits = Bits::default();
    let mut survivors = 0;

    for_each_block(db, 0..=height.0, "generated", |canonical| {
        let block_height = canonical.height;
        for (location, output) in outputs_in_order(&canonical.block, block_height) {
            let retained = match utxos.next_if(|(next, _)| *next <= location) {
                None => false,
                Some((next, _)) if next != location => {
                    bail!("unmatched UTXO at {next:?}");
                }
                Some((_, entry)) => {
                    ensure!(
                        !block_height.is_min(),
                        "genesis outputs must not enter the UTXO set"
                    );
                    let tx_index = location.transaction_index().as_usize();
                    ensure!(
                        entry == OrderedUtxo::new(output, block_height, tx_index),
                        "UTXO entry at {location:?} differs from its creating transaction"
                    );
                    survivors += 1;
                    true
                }
            };
            bits.push(retained);
        }
        Ok(())
    })?;
    ensure!(
        utxos.next().is_none(),
        "UTXO set contains entries beyond the canonical outputs"
    );

    let mut encoder = ArtifactEncoder::new(genesis, height, hash, bits.len)?;
    for bit in bits.iter() {
        encoder.push(bit)?;
    }
    let bytes = encoder.finish()?;

    // The encoder and parser are independent: check that the result parses.
    let parsed = ParsedArtifact::parse(Arc::from(bytes.as_slice()))?;
    ensure!(
        parsed.header().output_count == bits.len,
        "encoded output count differs"
    );
    ensure!(
        exact_boundary(db, network, height, hash)? == genesis,
        "archive changed during generation"
    );

    let sha256 = Sha256::digest(&bytes).into();
    Ok(Generated {
        bytes,
        height,
        hash,
        output_count: bits.len,
        survivors,
        sha256,
    })
}

/// Every transparent output of `block`, with its location, in canonical artifact order.
pub(super) fn outputs_in_order(
    block: &Block,
    height: Height,
) -> impl Iterator<Item = (OutputLocation, transparent::Output)> + '_ {
    block
        .transactions
        .iter()
        .enumerate()
        .flat_map(move |(tx_index, tx)| {
            tx.outputs()
                .into_iter()
                .enumerate()
                .map(move |(output_index, output)| {
                    let location = OutputLocation::from_usize(height, tx_index, output_index);
                    (location, output)
                })
        })
}

/// The `MaxCheckpoint` pin for `generated`, as the Rust constant a release PR adds to
/// `zebra_chain::parameters::spentness_hints`.
pub(super) fn pin_source(generated: &Generated) -> String {
    format!(
        "Some(MaxCheckpoint {{\n    \
             height: block::Height({}),\n    \
             // {}\n    \
             hash: block::Hash({}),\n    \
             spentness_hash: {},\n\
         }})",
        generated.height.0,
        generated.hash,
        byte_array(&generated.hash.0),
        byte_array(&generated.sha256),
    )
}

/// `bytes` as a Rust array literal.
fn byte_array(bytes: &[u8; 32]) -> String {
    let bytes: Vec<_> = bytes.iter().map(|byte| format!("0x{byte:02x}")).collect();
    format!("[{}]", bytes.join(", "))
}

/// Writes the artifact, then prints a summary and the pin for review.
pub(super) fn run(
    network: &Network,
    state: &Path,
    height: Height,
    block_hash: block::Hash,
    output: PathBuf,
) -> Result<()> {
    let generated = generate(&open(state, network)?, network, height, block_hash)?;
    write(output, &generated.bytes)?;
    println!(
        "height={} hash={} outputs={} survivors={} bytes={} sha256={}",
        generated.height.0,
        generated.hash,
        generated.output_count,
        generated.survivors,
        generated.bytes.len(),
        hex::encode(generated.sha256),
    );
    println!("{}", pin_source(&generated));
    Ok(())
}
