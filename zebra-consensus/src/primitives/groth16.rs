use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use tower::Service;

use zebra_chain::primitives::Groth16Proof;

use crate::BoxError;

/// Provides verification of Groth16 proofs for a specific statement.
///
/// Groth16 proofs require a proof verification key; the [`Verifier`] type is
/// responsible for ownership of the PVK.
pub struct Verifier {
    // XXX this needs to hold on to a verification key
}

impl Verifier {
    /// Create a new Groth16 verifier, supplying the encoding of the verification key.
    pub fn new(_encoded_verification_key: &[u8]) -> Result<Self, BoxError> {
        // parse and turn into a bellman type,
        // so that users don't have to have the entire bellman api
        unimplemented!();
    }
}

// XXX this is copied from the WIP batch bellman impl,
// in the future, replace with a re export

pub struct Item {
    pub proof: Groth16Proof,
    pub public_inputs: Vec<jubjub::Fr>,
}

// XXX in the future, Verifier will implement
// Service<BatchControl<Item>>> and be wrapped in a Batch
// to get a Service<Item>
// but for now, just implement Service<Item> and do unbatched verif.
//impl Service<BatchControl<Item>> for Verifier {
impl Service<Item> for Verifier {
    type Response = ();
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Item) -> Self::Future {
        unimplemented!()
    }
}
