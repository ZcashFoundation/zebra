use std::{
    collections::HashMap,
    convert::TryFrom,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use rand::thread_rng;
use rand_core::{CryptoRng, RngCore};
use tokio::sync::watch::{channel, Receiver, Sender};
use tower::{Service, ServiceExt};

use zebra_chain::redjubjub::{
    self, Error, PublicKey, PublicKeyBytes, SecretKey, Signature, SpendAuth,
};

type Scalar = zebra_chain::redjubjub::Randomizer;

/// A batch verification request.
///
/// This has two variants, to allow manually flushing queued verification
/// requests, even when the batching service is wrapped in other `tower` layers.
pub enum Request<'msg> {
    /// Request verification of this key-sig-message tuple.
    Verify(PublicKeyBytes<SpendAuth>, Signature<SpendAuth>, &'msg [u8]),
    /// Flush the current batch, computing all queued verification requests.
    Flush,
}

/// Lets us manage tuples of public key bytes, signatures, and
/// messages and massage them into the appropriate
impl<'msg, M: AsRef<[u8]> + ?Sized> From<(PublicKeyBytes<SpendAuth>, Signature<SpendAuth>, &'msg M)>
    for Request<'msg>
{
    fn from(tup: (PublicKeyBytes<SpendAuth>, Signature<SpendAuth>, &'msg M)) -> Request<'msg> {
        Request::Verify(tup.0, tup.1, tup.2.as_ref())
    }
}

/// Performs singleton RedJubjub signature verification.
///
/// This wraps the normal single-signature verification functions in a
/// [`Service`] implementation, allowing users to abstract over singleton and
/// batch verification.
#[derive(Default)]
pub struct SingletonVerifier;

impl Service<Request<'_>> for SingletonVerifier {
    type Response = ();
    type Error = redjubjub::Error;
    type Future = futures::future::Ready<Result<(), redjubjub::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        futures::future::ready(match req {
            Request::Verify(pk_bytes, sig, msg) => {
                PublicKey::<SpendAuth>::try_from(pk_bytes).and_then(|pk| pk.verify(msg, &sig))
            }
            Request::Flush => Ok(()),
        })
    }
}

/// Performs batch RedJubjub verification.
pub struct BatchVerifier {
    tx: Sender<Result<(), Error>>,
    rx: Receiver<Result<(), Error>>,
    /// The number of signatures per batch.
    batch_size: usize,
    /// The number of signatures currently queued for verification.
    num_sigs: usize,
    /// Signature data queued for verification.
    signatures: HashMap<PublicKeyBytes<SpendAuth>, Vec<(Scalar, Signature<SpendAuth>)>>,
}

#[cfg(test)]
mod tests {

    use tokio::runtime::Runtime;

    use super::*;

    async fn sign_and_verify<S>(svc: &mut S) -> impl std::future::Future<Output = Result<(), Error>>
    where
        for<'msg> S: Service<Request<'msg>, Response = (), Error = Error>,
    {
        let sk = SecretKey::<SpendAuth>::new(thread_rng());
        let pk_bytes = PublicKey::from(&sk).into();

        let msg = b"";
        let sig = sk.sign(thread_rng(), msg);

        svc.ready().await.unwrap();
        svc.call((pk_bytes, sig, msg).into())
    }

    #[test]
    fn singleton_verification() {
        let mut rt = Runtime::new().unwrap();
        rt.block_on(async {
            let mut svc = SingletonVerifier;
            let fut1 = sign_and_verify(&mut svc).await;
            let fut2 = sign_and_verify(&mut svc).await;
            let result1 = fut1.await;
            let result2 = fut2.await;
            assert_eq!(result1, Ok(()));
            assert_eq!(result2, Ok(()));
        })
    }

    // TODO: add proptests to test that singleton and batch of size
    // (1) both validate or fail together
}
