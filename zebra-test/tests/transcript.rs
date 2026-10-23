//! Tests for the [`zebra_test::transcript`] module.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use color_eyre::{
    config::{HookBuilder, Theme},
    Section as _,
};
use futures::{future::BoxFuture, FutureExt};
use insta::assert_snapshot;
use regex::Regex;
use tokio::time::{sleep, timeout, Sleep};
use tower::{Service, ServiceExt};

use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

const TRANSCRIPT_DATA: [(&str, Result<&str, ExpectedTranscriptError>); 4] = [
    ("req1", Ok("rsp1")),
    ("req2", Ok("rsp2")),
    ("req3", Ok("rsp3")),
    ("req4", Ok("rsp4")),
];

#[tokio::test]
async fn transcript_returns_responses_and_ends() {
    let _init_guard = zebra_test::init();

    let mut svc = Transcript::from(TRANSCRIPT_DATA.iter().cloned());

    for (req, rsp) in TRANSCRIPT_DATA.iter() {
        assert_eq!(
            svc.ready().await.unwrap().call(req).await.unwrap(),
            *rsp.as_ref().unwrap()
        );
    }
    assert!(svc.ready().await.unwrap().call("end").await.is_err());
}

#[tokio::test]
async fn transcript_errors_wrong_request() {
    let _init_guard = zebra_test::init();

    let mut svc = Transcript::from(TRANSCRIPT_DATA.iter().cloned());

    assert_eq!(
        svc.ready().await.unwrap().call("req1").await.unwrap(),
        "rsp1",
    );
    assert!(svc.ready().await.unwrap().call("bad").await.is_err());
}

/// Wrong requests retain their expected and actual values in the error diagnostic.
#[tokio::test]
async fn transcript_rejects_wrong_request_for_error_entry() {
    // Keep machine-specific context and terminal styling out of the snapshot.
    HookBuilder::default()
        .theme(Theme::new())
        .display_env_section(false)
        .capture_span_trace_by_default(false)
        .install()
        .expect("nextest runs each test in a separate process");

    let _init_guard = zebra_test::init();

    let mut svc = Transcript::from([("expected", Err::<(), _>(ExpectedTranscriptError::Any))]);
    let error = svc
        .call("wrong")
        .await
        .unwrap_err()
        .suppress_backtrace(true);
    let diagnostic = format!("{error:?}");
    // The `track-caller` feature of color-eyre, which `zebrad/error-debug` turns on, adds the
    // caller's file and line. The hook can only hide it when that feature is on, so drop it here.
    let diagnostic = Regex::new(r"\n\nLocation:\n[^\n]*")
        .expect("the pattern is valid")
        .replace(&diagnostic, "");

    assert_snapshot!(diagnostic.trim(), @r#"
    0: received unexpected request

    Expected Request:
       "expected"

    Found Request:
       "wrong"
    "#);
}

#[tokio::test]
async fn self_check() {
    let _init_guard = zebra_test::init();

    let t1 = Transcript::from(TRANSCRIPT_DATA.iter().cloned());
    let t2 = Transcript::from(TRANSCRIPT_DATA.iter().cloned());
    assert!(t1.check(t2).await.is_ok());
}

const TRANSCRIPT_DATA2: [(&str, Result<&str, ExpectedTranscriptError>); 4] = [
    ("req1", Ok("rsp1")),
    ("req2", Ok("rsp2")),
    ("req3", Ok("rsp3")),
    ("req4", Err(ExpectedTranscriptError::Any)),
];

#[tokio::test]
async fn self_check_err() {
    let _init_guard = zebra_test::init();

    let t1 = Transcript::from(TRANSCRIPT_DATA2.iter().cloned());
    let t2 = Transcript::from(TRANSCRIPT_DATA2.iter().cloned());
    t1.check(t2)
        .await
        .expect("transcript acting as the mocker and verifier should always pass")
}

struct DelayedService {
    ready: Pin<Box<Sleep>>,
    response_delay: Duration,
}

impl Service<&'static str> for DelayedService {
    type Response = &'static str;
    type Error = tower::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.ready.as_mut().poll(cx).map(Ok)
    }

    fn call(&mut self, _request: &'static str) -> Self::Future {
        let response_delay = self.response_delay;
        async move {
            sleep(response_delay).await;
            Ok("response")
        }
        .boxed()
    }
}

#[tokio::test(start_paused = true)]
async fn transcript_step_deadline_includes_readiness_and_response() {
    let _init_guard = zebra_test::init();

    for (readiness_seconds, response_seconds) in [(120, 0), (0, 120), (40, 40)] {
        let service = DelayedService {
            ready: Box::pin(sleep(Duration::from_secs(readiness_seconds))),
            response_delay: Duration::from_secs(response_seconds),
        };
        let transcript = Transcript::from([("slow request", Ok("response"))]);
        let error = timeout(Duration::from_secs(75), transcript.check(service))
            .await
            .expect("each transcript step must finish within its deadline")
            .unwrap_err();
        let diagnostic = error.to_string();

        assert!(
            diagnostic.contains("transcript timed out for request: \"slow request\""),
            "missing timeout context for readiness={readiness_seconds}s, \
             response={response_seconds}s: {error:?}",
        );
    }
}
