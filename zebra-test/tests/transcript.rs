// Standard lints
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![deny(clippy::await_holding_lock)]
#![forbid(unsafe_code)]

use tower::{Service, ServiceExt};
use zebra_test::transcript::TransError;
use zebra_test::transcript::Transcript;

const TRANSCRIPT_DATA: [(&str, Result<&str, TransError>); 4] = [
    ("req1", Ok("rsp1")),
    ("req2", Ok("rsp2")),
    ("req3", Ok("rsp3")),
    ("req4", Ok("rsp4")),
];

#[tokio::test]
async fn transcript_returns_responses_and_ends() {
    zebra_test::init();

    let mut svc = Transcript::from(TRANSCRIPT_DATA.iter().cloned());

    for (req, rsp) in TRANSCRIPT_DATA.iter() {
        assert_eq!(
            svc.ready_and().await.unwrap().call(req).await.unwrap(),
            *rsp.as_ref().unwrap()
        );
    }
    assert!(svc.ready_and().await.unwrap().call("end").await.is_err());
}

#[tokio::test]
async fn transcript_errors_wrong_request() {
    zebra_test::init();

    let mut svc = Transcript::from(TRANSCRIPT_DATA.iter().cloned());

    assert_eq!(
        svc.ready_and().await.unwrap().call("req1").await.unwrap(),
        "rsp1",
    );
    assert!(svc.ready_and().await.unwrap().call("bad").await.is_err());
}

#[tokio::test]
async fn self_check() {
    zebra_test::init();

    let t1 = Transcript::from(TRANSCRIPT_DATA.iter().cloned());
    let t2 = Transcript::from(TRANSCRIPT_DATA.iter().cloned());
    assert!(t1.check(t2).await.is_ok());
}

#[derive(Debug, thiserror::Error)]
#[error("Error")]
struct Error;

const TRANSCRIPT_DATA2: [(&str, Result<&str, TransError>); 4] = [
    ("req1", Ok("rsp1")),
    ("req2", Ok("rsp2")),
    ("req3", Ok("rsp3")),
    ("req4", Err(TransError::Any)),
];

#[tokio::test]
async fn self_check_err() {
    zebra_test::init();

    let t1 = Transcript::from(TRANSCRIPT_DATA2.iter().cloned());
    let t2 = Transcript::from(TRANSCRIPT_DATA2.iter().cloned());
    t1.check(t2)
        .await
        .expect("transcript acting as the mocker and verifier should always pass")
}
