//! Event-driven wait helpers for Zakura tests.

use std::{error::Error, fmt, time::Duration};

use tokio::time::timeout;

/// Error returned when an event-driven wait times out.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WaitError {
    description: String,
    timeout: Duration,
}

impl WaitError {
    /// Create a timeout error with a useful predicate description.
    pub fn new(description: impl Into<String>, timeout: Duration) -> Self {
        Self {
            description: description.into(),
            timeout,
        }
    }
}

impl fmt::Display for WaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "timed out after {:?} waiting for {}",
            self.timeout, self.description
        )
    }
}

impl Error for WaitError {}

/// Poll `predicate` until it becomes true or `max_wait` elapses.
pub async fn await_until(
    description: impl Into<String>,
    max_wait: Duration,
    mut predicate: impl FnMut() -> bool,
) -> Result<(), WaitError> {
    if predicate() {
        return Ok(());
    }

    let description = description.into();
    let result = timeout(max_wait, async {
        loop {
            tokio::task::yield_now().await;
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(_) if predicate() => Ok(()),
        Err(_) => Err(WaitError::new(description, max_wait)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use super::*;

    #[tokio::test]
    async fn await_until_returns_when_predicate_flips() {
        let flag = Arc::new(AtomicBool::new(false));
        let setter = flag.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            setter.store(true, Ordering::SeqCst);
        });

        await_until("flag", Duration::from_secs(1), || {
            flag.load(Ordering::SeqCst)
        })
        .await
        .expect("flag should flip");
    }

    #[tokio::test]
    async fn await_until_reports_predicate_on_timeout() {
        let error = await_until("never true", Duration::from_millis(1), || false)
            .await
            .expect_err("predicate never becomes true");

        assert!(error.to_string().contains("never true"));
    }
}
