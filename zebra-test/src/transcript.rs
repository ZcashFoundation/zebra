//! A [`Service`](tower::Service) implementation based on a fixed transcript.

use std::{
    fmt::Debug,
    sync::Arc,
    task::{Context, Poll},
};

use color_eyre::{
    eyre::{eyre, Report, WrapErr},
    section::Section,
    section::SectionExt,
};
use futures::future::{ready, Ready};
use tower::{Service, ServiceExt};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// An error-checking function: is the value an expected error?
///
/// If the checked error is the expected error, the function should return `Ok(())`.
/// Otherwise, it should just return the checked error, wrapped inside `Err`.
pub type ErrorChecker = fn(Option<BoxError>) -> Result<(), BoxError>;

/// An expected error in a transcript.
#[derive(Debug, Clone)]
pub enum ExpectedTranscriptError {
    /// Match any error
    Any,
    /// Use a validator function to check for matching errors
    Exact(Arc<ErrorChecker>),
}

impl ExpectedTranscriptError {
    /// Convert the `verifier` function into an exact error checker
    pub fn exact(verifier: ErrorChecker) -> Self {
        ExpectedTranscriptError::Exact(verifier.into())
    }

    /// Check the actual error `e` against this expected error.
    #[track_caller]
    fn check(&self, e: BoxError) -> Result<(), Report> {
        match self {
            ExpectedTranscriptError::Any => Ok(()),
            ExpectedTranscriptError::Exact(checker) => checker(Some(e)),
        }
        .map_err(ErrorCheckerError)
        .wrap_err("service returned an error but it didn't match the expected error")
    }

    fn mock(&self) -> Report {
        match self {
            ExpectedTranscriptError::Any => eyre!("mock error"),
            ExpectedTranscriptError::Exact(checker) => {
                checker(None).map_err(|e| eyre!(e)).expect_err(
                    "transcript should correctly produce the expected mock error when passed None",
                )
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("ErrorChecker Error: {0}")]
struct ErrorCheckerError(BoxError);

/// A transcript: a list of requests and expected results.
#[must_use]
pub struct Transcript<R, S, I>
where
    I: Iterator<Item = (R, Result<S, ExpectedTranscriptError>)>,
{
    messages: I,
}

impl<R, S, I> From<I> for Transcript<R, S, I::IntoIter>
where
    I: IntoIterator<Item = (R, Result<S, ExpectedTranscriptError>)>,
{
    fn from(messages: I) -> Self {
        Self {
            messages: messages.into_iter(),
        }
    }
}

impl<R, S, I> Transcript<R, S, I>
where
    I: Iterator<Item = (R, Result<S, ExpectedTranscriptError>)>,
    R: Debug,
    S: Debug + Eq,
{
    /// Check this transcript against the responses from the `to_check` service
    pub async fn check<C>(mut self, mut to_check: C) -> Result<(), Report>
    where
        C: Service<R, Response = S>,
        C::Error: Into<BoxError>,
    {
        for (req, expected_rsp) in &mut self.messages {
            // These unwraps could propagate errors with the correct
            // bound on C::Error
            let fut = to_check
                .ready()
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
                        .with_section(|| format!("{expected_rsp:?}").header("Expected Response:"))
                        .with_section(|| format!("{rsp:?}").header("Found Response:"))?;
                    }
                }
                (Ok(rsp), Err(error_checker)) => {
                    let error = Err(eyre!("received a response when an error was expected"))
                        .with_section(|| format!("{rsp:?}").header("Found Response:"));

                    let error = match std::panic::catch_unwind(|| error_checker.mock()) {
                        Ok(expected_err) => error
                            .with_section(|| format!("{expected_err:?}").header("Expected Error:")),
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
                        .with_section(|| format!("{expected_rsp:?}").header("Expected Response:"))?
                }
                (Err(e), Err(error_checker)) => {
                    error_checker.check(e.into())?;
                    continue;
                }
            }
        }
        Ok(())
    }
}

impl<R, S, I> Service<R> for Transcript<R, S, I>
where
    R: Debug + Eq,
    I: Iterator<Item = (R, Result<S, ExpectedTranscriptError>)>,
{
    type Response = S;
    type Error = Report;
    type Future = Ready<Result<S, Report>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[track_caller]
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
                                    format!("{expected_request:?}").header("Expected Request:")
                                })
                                .with_section(|| format!("{request:?}").header("Found Request:")),
                        )
                    }
                }
                Err(check_fn) => ready(Err(check_fn.mock())),
            }
        } else {
            ready(Err(eyre!("Got request after transcript ended")))
        }
    }
}
