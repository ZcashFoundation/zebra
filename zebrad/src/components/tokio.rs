//! A component owning the Tokio runtime.

use crate::prelude::*;
use abscissa_core::{Application, Component, FrameworkError, Shutdown};
use color_eyre::Report;
use std::future::Future;
use tokio::runtime::Runtime;

/// An Abscissa component which owns a Tokio runtime.
///
/// The runtime is stored as an `Option` so that when it's time to enter an async
/// context by calling `block_on` with a "root future", the runtime can be taken
/// independently of Abscissa's component locking system. Otherwise whatever
/// calls `block_on` holds an application lock for the entire lifetime of the
/// async context.
#[derive(Component, Debug)]
pub struct TokioComponent {
    pub rt: Option<Runtime>,
}

impl TokioComponent {
    pub fn new() -> Result<Self, FrameworkError> {
        Ok(Self {
            rt: Some(Runtime::new().unwrap()),
        })
    }
}

/// Zebrad's graceful shutdown function, blocks until one of the supported
/// shutdown signals is received.
async fn shutdown() {
    imp::shutdown().await;
}

/// Extension trait to centralize entry point for runnable subcommands that
/// depend on tokio
pub(crate) trait RuntimeRun {
    fn run(&mut self, fut: impl Future<Output = Result<(), Report>>);
}

impl RuntimeRun for Runtime {
    fn run(&mut self, fut: impl Future<Output = Result<(), Report>>) {
        let result = self.block_on(async move {
            tokio::select! {
                result = fut => result,
                _ = shutdown() => Ok(()),
            }
        });

        match result {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Error: {:?}", e);
                app_writer().shutdown(Shutdown::Forced);
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use tokio::signal::unix::{signal, SignalKind};
    use tracing::info;

    pub(super) async fn shutdown() {
        tokio::select! {
            // SIGINT  - To allow Ctrl-c to emulate SIGTERM while developing.
            () = sig(SignalKind::interrupt(), "SIGINT") => {}
            // SIGTERM - Kubernetes sends this to start a graceful shutdown.
            () = sig(SignalKind::terminate(), "SIGTERM") => {}
        };
    }

    async fn sig(kind: SignalKind, name: &'static str) {
        // Create a Future that completes the first
        // time the process receives 'sig'.
        signal(kind)
            .expect("Failed to register signal handler")
            .recv()
            .await;

        info!(
            // use target to remove 'imp' from output
            target: "zebrad::signal",
            "received {}, starting shutdown",
            name,
        );
    }
}

#[cfg(not(unix))]
mod imp {
    use futures::prelude::*;
    use tracing::info;

    pub(super) async fn shutdown() {
        // On Windows, we don't have all the signals, but Windows also
        // isn't our expected deployment target. This implementation allows
        // developers on Windows to simulate proxy graceful shutdown
        // by pressing Ctrl-C.
        tokio::signal::ctrl_c()
            .await
            .expect("listening for ctrl-c signal should never fail");

        info!(
            // use target to remove 'imp' from output
            target: "zebrad::signal",
            "received Ctrl-C, starting shutdown",
        );
    }
}
