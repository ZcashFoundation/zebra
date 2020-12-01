use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use tower::timeout::Timeout;
use tracing::Instrument;

use zebra_chain::{parameters::NetworkUpgrade, transaction::Transaction, transparent};
use zebra_state::Utxo;

use crate::BoxError;

/// A timeout applied to UTXO lookup requests.
///
/// The exact value is non-essential, but this should be long enough to allow
/// out-of-order verification of blocks (UTXOs are not required to be ready
/// immediately) while being short enough to:
///   * prune blocks that are too far in the future to be worth keeping in the
///     queue,
///   * fail blocks that reference invalid UTXOs, and
///   * fail blocks that reference UTXOs from blocks that have temporarily failed
///     to download, because a peer sent Zebra a bad list of block hashes. (The
///     UTXO verification failure will restart the sync, and re-download the
///     chain in the correct order.)
const UTXO_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3 * 60);

/// Asynchronous script verification.
///
/// The verifier asynchronously requests the UTXO a transaction attempts
/// to use as an input, and verifies the script as soon as it becomes
/// available.  This allows script verification to be performed
/// asynchronously, rather than requiring that the entire chain up to
/// the previous block is ready.
///
/// The asynchronous script verification design is documented in [RFC4].
///
/// [RFC4]: https://zebra.zfnd.org/dev/rfcs/0004-asynchronous-script-verification.html
#[derive(Debug, Clone)]
pub struct Verifier<ZS> {
    state: Timeout<ZS>,
}

impl<ZS> Verifier<ZS> {
    pub fn new(state: ZS) -> Self {
        Self {
            state: Timeout::new(state, UTXO_LOOKUP_TIMEOUT),
        }
    }
}

/// A script verification request.
#[derive(Debug)]
pub struct Request {
    /// Ideally, this would supply only an `Outpoint` and the unlock script,
    /// rather than the entire `Transaction`, but we call a C++
    /// implementation, and its FFI requires the entire transaction.
    ///
    /// This causes quadratic script verification behavior, so
    /// at some future point, we need to reform this data.
    pub transaction: Arc<Transaction>,
    pub input_index: usize,
    /// A set of additional UTXOs known in the context of this verification request.
    ///
    /// This allows specifying additional UTXOs that are not already known to the chain state.
    pub known_utxos: Arc<HashMap<transparent::OutPoint, Utxo>>,
    /// The network upgrade active in the context of this verification request.
    ///
    /// Because the consensus branch ID changes with each network upgrade,
    /// it has to be specified on a per-request basis.
    pub upgrade: NetworkUpgrade,
}

impl<ZS> tower::Service<Request> for Verifier<ZS>
where
    ZS: tower::Service<zebra_state::Request, Response = zebra_state::Response, Error = BoxError>,
    ZS::Future: Send + 'static,
{
    type Response = ();
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.state.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        use futures_util::FutureExt;

        let Request {
            transaction,
            input_index,
            known_utxos,
            upgrade,
        } = req;
        let input = &transaction.inputs()[input_index];
        let branch_id = upgrade
            .branch_id()
            .expect("post-Sapling NUs have a consensus branch ID");

        match input {
            transparent::Input::PrevOut { outpoint, .. } => {
                let outpoint = *outpoint;

                let span = tracing::trace_span!("script", ?outpoint);
                let query =
                    span.in_scope(|| self.state.call(zebra_state::Request::AwaitUtxo(outpoint)));

                async move {
                    tracing::trace!("awaiting outpoint lookup");
                    let utxo = if let Some(output) = known_utxos.get(&outpoint) {
                        tracing::trace!("UXTO in known_utxos, discarding query");
                        output.clone()
                    } else if let zebra_state::Response::Utxo(utxo) = query.await? {
                        utxo
                    } else {
                        unreachable!("AwaitUtxo always responds with Utxo")
                    };
                    tracing::trace!(?utxo, "got UTXO");

                    if transaction.inputs().len() < 20 {
                        zebra_script::is_valid(
                            transaction,
                            branch_id,
                            (input_index as u32, utxo.output),
                        )?;
                        tracing::trace!("script verification succeeded");
                    } else {
                        tracing::debug!(
                            inputs.len = transaction.inputs().len(),
                            "skipping verification of script with many inputs to avoid quadratic work until we fix zebra_script/zcash_script interface"
                        );
                    }

                    Ok(())
                }
                .instrument(span)
                .boxed()
            }
            transparent::Input::Coinbase { .. } => {
                async { Err("unexpected coinbase input".into()) }.boxed()
            }
        }
    }
}
