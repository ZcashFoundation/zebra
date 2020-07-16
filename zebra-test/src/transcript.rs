//! A [`Service`](tower::Service) implementation based on a fixed transcript.

use color_eyre::{
    eyre::{eyre, Report, WrapErr},
    section::Section,
    section::SectionExt,
};
use futures::future::{ready, Ready};
use std::{
    fmt::Debug,
    task::{Context, Poll},
};
use tower::{Service, ServiceExt};

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A function for validating or constructing errors for `Transcript` responses
///
/// # Details
///
/// This function serves dual purposes.
///
/// * When a `Transcript` is being used as a validator the ErrorChecker is used
///   to validate the errors returned by the service that is being checked.
/// * When the `Transcript` is being used as a Mock Service the ErrorChecker is
///   used to _produce_ the error that it would otherwise have expected to receive.
///
/// The input `Option` is used to differentiate between these two roles. When the
/// input is `Some(error)` the function should validate the error. When the input
/// is `None` the function should produce the expected error. It is okay to leave
/// functionality you won't use unimplemented, e.g. if you only need to validate
/// a service it's okay to unwrap the input and ignore the mocking functionality.
///
/// ## Validating Errors
///
/// When validating errors you should return `Ok(())` if the input error was the
/// expected error. If the input error was unexpected you can return any error
/// you want to indicate what went wrong.
///
/// ## Mocking Error Respones
///
/// When acting as a mock service your `ErrorChecker` should produce the expected
/// error whenever the input is `None`.
///
/// # Example
///
/// ```rust
/// const TRANSCRIPT_DATA2: [(&str, Result<&str, ErrorChecker>); 4] = [
///     ("req1", Ok("rsp1")),
///     ("req2", Ok("rsp2")),
///     ("req3", Ok("rsp3")),
///     (
///         "req4",
///         Err(|e| {
///             if e.is_none() {
///                 Err("this is bad")?;
///             }
///
///             let e = e.unwrap();
///
///             if e.to_string() == "this is bad" {
///                 Ok(())
///             } else {
///                 Err(e)
///             }
///         }),
///     ),
/// ];
pub type ErrorChecker = fn(Option<Error>) -> Result<(), Error>;

#[derive(Debug, thiserror::Error)]
#[error("ErrorChecker Error: {0}")]
struct ErrorCheckerError(Error);

pub struct Transcript<R, S, E, I>
where
    I: Iterator<Item = (R, Result<S, E>)>,
{
    messages: I,
}

impl<R, S, E, I> From<I> for Transcript<R, S, E, I>
where
    I: Iterator<Item = (R, Result<S, E>)>,
{
    fn from(messages: I) -> Self {
        Self { messages }
    }
}

impl<R, S, E, I> Transcript<R, S, E, I>
where
    I: Iterator<Item = (R, Result<S, E>)>,
    R: Debug,
    S: Debug + Eq,
    E: Fn(Option<Error>) -> Result<(), Error> + std::panic::RefUnwindSafe,
{
    pub async fn check<C>(mut self, mut to_check: C) -> Result<(), Report>
    where
        C: Service<R, Response = S>,
        C::Error: Into<Error>,
    {
        while let Some((req, expected_rsp)) = self.messages.next() {
            // These unwraps could propagate errors with the correct
            // bound on C::Error
            let fut = to_check
                .ready_and()
                .await
                .map_err(Into::into)
                .map_err(|e| eyre!(e))
                .expect("expected service to not fail during execution of transcript");

            let response = fut.call(req).await;

            match (response, expected_rsp) {
                (Ok(rsp), Ok(expected_rsp)) => {
                    if rsp != expected_rsp {
                        Err(eyre!(
                            "response doesn't match transcript's expected response"
                        ))
                        .with_section(|| format!("{:?}", expected_rsp).header("Expected Response:"))
                        .with_section(|| format!("{:?}", rsp).header("Found Response:"))?;
                    }
                }
                (Ok(rsp), Err(error_checker)) => {
                    let error = Err(eyre!("received a response when an error was expected"))
                        .with_section(|| format!("{:?}", rsp).header("Found Response:"));

                    let error = match std::panic::catch_unwind(|| error_checker(None)) {
                        Ok(expected_rsp) => error.with_section(|| {
                            format!("{:?}", expected_rsp).header("Expected Response:")
                        }),
                        Err(pi) => {
                            let payload = pi
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| pi.downcast_ref::<&str>().map(ToString::to_string))
                                .unwrap_or_else(|| "<non string panic payload>".into());

                            error
                                .section(payload.header("Panic:"))
                                .wrap_err("ErrorChecker panicked when producing expected response")
                        }
                    };

                    error?;
                }
                (Err(e), Ok(expected_rsp)) => {
                    Err(eyre!("received an error when a response was expected"))
                        .with_error(|| ErrorCheckerError(e.into()))
                        .with_section(|| {
                            format!("{:?}", expected_rsp).header("Expected Response:")
                        })?
                }
                (Err(e), Err(error_checker)) => {
                    error_checker(Some(e.into()))
                        .map_err(ErrorCheckerError)
                        .wrap_err(
                            "service returned an error but it didn't match the expected error",
                        )?;
                    continue;
                }
            }
        }
        Ok(())
    }
}

const TRANSCRIPT_MOCK_GUIDE: &str = r#"
Transcripts that are mocking services that produce errors must construct the
errors they expect in their error handling closure.

When a transcript encounters an expected error it passes in a `None` which is
the signal to `check_fn` that it should construct the expected Error.

# Example

```rust
const TRANSCRIPT_DATA2: [(&str, Result<&str, ErrorChecker>); 4] = [
    ("req1", Ok("rsp1")),
    ("req2", Ok("rsp2")),
    ("req3", Ok("rsp3")),
    (
        "req4",
        Err(|e| {
            if e.is_none() {
                Err("this is bad")?;
            }

            let e = e.unwrap();

            if e.to_string() == "this is bad" {
                Ok(())
            } else {
                Err(e)
            }
        }),
    ),
];
```
"#;

impl<R, S, E, I> Service<R> for Transcript<R, S, E, I>
where
    R: Debug + Eq,
    I: Iterator<Item = (R, Result<S, E>)>,
    E: Fn(Option<Error>) -> Result<(), Error>,
{
    type Response = S;
    type Error = Report;
    type Future = Ready<Result<S, Report>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: R) -> Self::Future {
        if let Some((expected_request, response)) = self.messages.next() {
            match response {
                Ok(response) => {
                    if request == expected_request {
                        ready(Ok(response))
                    } else {
                        ready(
                            Err(eyre!("received unexpected request"))
                                .with_section(|| {
                                    format!("{:?}", expected_request).header("Expected Request:")
                                })
                                .with_section(|| format!("{:?}", request).header("Found Request:")),
                        )
                    }
                }
                Err(check_fn) => {
                    // transcripts that mock errors must handle this and return the mocked error
                    let err = match check_fn(None) {
                        Ok(()) => Err(eyre!("transcript incorrectly handled mock error"))
                            .section(
                                TRANSCRIPT_MOCK_GUIDE.header("Transcript Error Mocking Guide:"),
                            )
                            .unwrap(),
                        Err(e) => eyre!(e),
                    };

                    ready(Err(err))
                }
            }
        } else {
            ready(Err(eyre!("Got request after transcript ended")))
        }
    }
}
