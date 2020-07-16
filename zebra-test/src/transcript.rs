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
    E: Fn(Error) -> Result<(), Error>,
{
    pub async fn check<C>(mut self, mut to_check: C) -> Result<(), Report>
    where
        C: Service<R, Response = S>,
        C::Error: Into<Error>,
    {
        while let Some((req, expected_rsp)) = self.messages.next() {
            // These unwraps could propagate errors with the correct
            // bound on C::Error
            let fut = to_check.ready_and().await;

            let fut = match fut {
                Ok(fut) => Ok(fut),
                Err(e) => match &expected_rsp {
                    Ok(expected_rsp) => Err(eyre!(e.into()))
                        .wrap_err("service's ready_and returned an error: transcript expected an response")
                        .with_section(|| {
                            format!("{:?}", expected_rsp).header("Expected Response:")
                        }),
                    Err(error_checker) => {
                        error_checker(e.into())
                            .map_err(|e| eyre!(e))
                            .wrap_err("service returned an error but it didn't match the expected error")?;
                        continue;
                    },
                },
            }?;

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
                (Ok(rsp), Err(_)) => {
                    todo!("got response when an error was expected. rsp={:?}", rsp)
                }
                (Err(_), Ok(expected_rsp)) => todo!(
                    "got an error when a response was expected. expected_rsp={:?}",
                    expected_rsp
                ),
                (Err(e), Err(error_checker)) => {
                    error_checker(e.into()).map_err(|e| eyre!(e)).wrap_err(
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

When a transcript encounters an expected error it passes in a `MockError`
which the `check_fn` should then map to the desired error type.

# Example

```rust
static EXAMPLE_TRANSCRIPT: Lazy<Vec<(Request, Response)>> = Lazy::new(|| {
    let block = zebra_test::vectors::BLOCK_MAINNET_415000_BYTES
        .zcash_deserialize_into::<Arc<_>>()
        .unwrap();

    vec![
        (
            Request::AddBlock {
                block: block.clone(),
            },
            Err(|e| {
                if e.is::<MockError>() {
                    return Err(zebra_state::Error::ActualError);
                } else if !e.is::<zebra_state::Error>() {
                    todo!()
                } else {
                    Ok(())
                }
            })
        ),
    ]
});
```
"#;

#[derive(Debug, thiserror::Error)]
#[error("mock error which should be mapped to the desired mock error type")]
pub struct MockError;

impl<R, S, E, I> Service<R> for Transcript<R, S, E, I>
where
    R: Debug + Eq,
    I: Iterator<Item = (R, Result<S, E>)>,
    E: Fn(Error) -> Result<(), Error>,
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
                        ready(Err(eyre!(
                            "Expected {:?}, got {:?}",
                            expected_request,
                            request
                        )))
                    }
                }
                Err(check_fn) => {
                    // transcripts that mock errors must handle this and return the mocked error
                    let err = MockError.into();
                    let err = match check_fn(err) {
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
