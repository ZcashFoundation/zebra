//! Catchable run interruption: cancel waits, then use the ordinary bounded cleanup path.

use std::future::Future;

use tokio::{runtime::Runtime, sync::watch};
use zebra_state::BoxError;

pub(super) struct Interruption {
    reason: watch::Receiver<Option<&'static str>>,
}

impl Interruption {
    pub(super) fn new(runtime: &Runtime) -> Result<Self, BoxError> {
        let _entered = runtime.enter();
        let (sender, reason) = watch::channel(None);
        // Install both Unix handlers synchronously, before starting any child.
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            runtime.spawn(async move {
                let reason = tokio::select! {
                    _ = interrupt.recv() => "interrupted by SIGINT",
                    _ = terminate.recv() => "interrupted by SIGTERM",
                };
                sender.send_replace(Some(reason));
            });
        }
        #[cfg(not(unix))]
        runtime.spawn(async move {
            let reason = match tokio::signal::ctrl_c().await {
                Ok(()) => "interrupted by Ctrl-C",
                Err(_) => "Ctrl-C listener failed",
            };
            sender.send_replace(Some(reason));
        });
        Ok(Self { reason })
    }

    pub(super) fn check(&self) -> Result<(), BoxError> {
        match *self.reason.borrow() {
            Some(reason) => Err(reason.into()),
            None => Ok(()),
        }
    }

    pub(super) async fn until<T>(
        &mut self,
        work: impl Future<Output = Result<T, BoxError>>,
    ) -> Result<T, BoxError> {
        self.check()?;
        tokio::select! {
            biased;
            changed = self.reason.changed() => {
                changed.map_err(|_| "interruption listener stopped")?;
                Err(self.reason.borrow().unwrap_or("interruption listener stopped").into())
            }
            result = work => result,
        }
    }
}
