//! A [`Service`](tower::Service) implementation based on a fixed transcript.

use color_eyre::{
    eyre::{eyre, Report, WrapErr},
    section::Section,
    section::SectionExt,
};
use futures::future::{ready, Ready};
use std::{
    fmt::Debug,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Service, ServiceExt};

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

pub type ErrorChecker = fn(Option<Error>) -> Result<(), Error>;

#[derive(Debug, Clone)]
pub enum TransError {
    Any,
    Exact(Arc<ErrorChecker>),
}

impl TransError {
    pub fn exact(verifier: ErrorChecker) -> Self {
        TransError::Exact(verifier.into())
    }

    fn check(&self, e: Error) -> Result<(), Report> {
        match self {
            TransError::Any => Ok(()),
            TransError::Exact(checker) => checker(Some(e)),
        }
        .map_err(ErrorCheckerError)
        .wrap_err("service returned an error but it didn't match the expected error")
    }

    fn mock(&self) -> Report {
        match self {
            TransError::Any => eyre!("mock error"),
            TransError::Exact(checker) => checker(None).map_err(|e| eyre!(e)).expect_err(
                "transcript should correctly produce the expected mock error when passed None",
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("ErrorChecker Error: {0}")]
struct ErrorCheckerError(Error);

#[must_use]
pub struct Transcript<R, S, I>
where
    I: Iterator<Item = (R, Result<S, TransError>)>,
{
    messages: I,
}

impl<R, S, I> From<I> for Transcript<R, S, I::IntoIter>
where
    I: IntoIterator<Item = (R, Result<S, TransError>)>,
{
    fn from(messages: I) -> Self {
        Self {
            messages: messages.into_iter(),
        }
    }
}

impl<R, S, I> Transcript<R, S, I>
where
    I: Iterator<Item = (R, Result<S, TransError>)>,
    R: Debug,
    S: Debug + Eq,
{
    pub async fn check<C>(mut self, mut to_check: C) -> Result<(), Report>
    where
        C: Service<R, Response = S>,
        C::Error: Into<Error>,
    {
        for (req, expected_rsp) in &mut self.messages {
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

                    let error = match std::panic::catch_unwind(|| error_checker.mock()) {
                        Ok(expected_err) => error.with_section(|| {
                            format!("{:?}", expected_err).header("Expected Error:")
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
    I: Iterator<Item = (R, Result<S, TransError>)>,
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
                Err(check_fn) => ready(Err(check_fn.mock())),
            }
        } else {
            ready(Err(eyre!("Got request after transcript ended")))
        }
    }
}
