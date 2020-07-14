use tower::{service_fn, Service, ServiceExt};
use tower_fallback::{Either, Fallback};

#[tokio::test]
async fn fallback() {
    zebra_test::init();

    // we'd like to use Transcript here but it can't handle errors :(

    let svc1 = service_fn(|val: u64| async move {
        if val < 10 {
            Ok(val)
        } else {
            Err("too big value on svc1")
        }
    });
    let svc2 = service_fn(|val: u64| async move {
        if val < 20 {
            Ok(100 + val)
        } else {
            Err("too big value on svc2")
        }
    });

    let mut svc = Fallback::new(svc1, svc2);

    assert_eq!(svc.ready_and().await.unwrap().call(1).await, Ok(1));
    assert_eq!(svc.ready_and().await.unwrap().call(11).await, Ok(111));
    assert_eq!(
        svc.ready_and().await.unwrap().call(21).await,
        Err(Either::Right("too big value on svc2"))
    );
}
