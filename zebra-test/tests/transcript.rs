use tower::{Service, ServiceExt};

use zebra_test::transcript::Transcript;

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type ErrorChecker = fn(Error) -> Result<(), Error>;

const TRANSCRIPT_DATA: [(&str, Result<&str, ErrorChecker>); 4] = [
    ("req1", Ok("rsp1")),
    ("req2", Ok("rsp2")),
    ("req3", Ok("rsp3")),
    ("req4", Ok("rsp4")),
];

#[tokio::test]
async fn transcript_returns_responses_and_ends() {
    let mut svc = Transcript::from(TRANSCRIPT_DATA.iter().cloned());

    for (req, rsp) in TRANSCRIPT_DATA.iter() {
        assert_eq!(
            svc.ready_and().await.unwrap().call(req).await.unwrap(),
            rsp.unwrap()
        );
    }
    assert!(svc.ready_and().await.unwrap().call("end").await.is_err());
}

#[tokio::test]
async fn transcript_errors_wrong_request() {
    let mut svc = Transcript::from(TRANSCRIPT_DATA.iter().cloned());

    assert_eq!(
        svc.ready_and().await.unwrap().call("req1").await.unwrap(),
        "rsp1",
    );
    assert!(svc.ready_and().await.unwrap().call("bad").await.is_err());
}

#[tokio::test]
async fn self_check() {
    let t1 = Transcript::from(TRANSCRIPT_DATA.iter().cloned());
    let t2 = Transcript::from(TRANSCRIPT_DATA.iter().cloned());
    assert!(t1.check(t2).await.is_ok());
}
