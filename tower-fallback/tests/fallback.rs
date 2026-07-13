//! Tests for tower-fallback

use tower::{service_fn, Service, ServiceExt};
use tower_fallback::{BoxedError, Fallback};

#[tokio::test]
async fn fallback() {
    let _init_guard = zebra_test::init();

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

    assert_eq!(svc.ready().await.unwrap().call(1).await.unwrap(), 1);
    assert_eq!(svc.ready().await.unwrap().call(11).await.unwrap(), 111);
    assert!(svc.ready().await.unwrap().call(21).await.is_err());
}

#[tokio::test]
async fn custom_fallback_policy() {
    let _init_guard = zebra_test::init();

    let svc1 = service_fn(|val: u64| async move {
        if val < 10 {
            Ok::<_, &'static str>(Some(val))
        } else {
            Ok(None)
        }
    });
    let svc2 = service_fn(|val: u64| async move {
        if val < 20 {
            Ok(Some(100 + val))
        } else {
            Err("too big value on svc2")
        }
    });

    let mut svc =
        Fallback::new_with_policy(svc1, svc2, |result: &Result<Option<u64>, BoxedError>| {
            matches!(result, Ok(None))
        });

    assert_eq!(svc.ready().await.unwrap().call(1).await.unwrap(), Some(1));
    assert_eq!(
        svc.ready().await.unwrap().call(11).await.unwrap(),
        Some(111)
    );
    assert!(svc.ready().await.unwrap().call(21).await.is_err());
}
