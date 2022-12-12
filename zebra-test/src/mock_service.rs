//! Some helpers to make it simpler to mock Tower services.
//!
//! A [`MockService`] is a generic [`tower::Service`] implementation that allows intercepting
//! requests, responding to them individually, and checking that there are no requests to be
//! received (at least during a period of time). The [`MockService`] can be built for proptests or
//! for normal Rust unit tests.
//!
//! # Example
//!
//! ```
//! use zebra_test::mock_service::MockService;
//! # use tower::ServiceExt;
//!
//! # let reactor = tokio::runtime::Builder::new_current_thread()
//! #     .enable_all()
//! #     .build()
//! #     .expect("Failed to build Tokio runtime");
//! #
//! # reactor.block_on(async {
//! let mut mock_service = MockService::build().for_unit_tests();
//! let mut service = mock_service.clone();
//! #
//! # // Add types to satisfy the compiler's type inference for the `Error` type.
//! # let _typed_mock_service: MockService<_, _, _> = mock_service.clone();
//!
//! let call = tokio::spawn(mock_service.clone().oneshot("hello"));
//!
//! mock_service
//!     .expect_request("hello").await
//!     .respond("hi!");
//!
//! mock_service.expect_no_requests().await;
//!
//! let response = call
//!     .await
//!     .expect("Failed to run call on the background")
//!     .expect("Failed to receive response from service");
//!
//! assert_eq!(response, "hi!");
//! # });
//! ```

use std::{
    fmt::Debug,
    marker::PhantomData,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{future::BoxFuture, FutureExt};
use proptest::prelude::*;
use tokio::{
    sync::{
        broadcast::{self, error::RecvError},
        oneshot, Mutex,
    },
    time::timeout,
};
use tower::{BoxError, Service};

/// The default size of the channel that forwards received requests.
///
/// If requests are received faster than the test code can consume them, some requests may be
/// ignored.
///
/// This value can be configured in the [`MockService`] using
/// [`MockServiceBuilder::with_proxy_channel_size`].
const DEFAULT_PROXY_CHANNEL_SIZE: usize = 100;

/// The default timeout before considering a request has not been received.
///
/// This is the time that the mocked service waits before considering a request will not be
/// received. It can be configured in the [`MockService`] using
/// [`MockServiceBuilder::with_max_request_delay`].
///
/// Note that if a test checks that no requests are received, each check has to wait for this
/// amount of time, so this may affect the test execution time.
///
/// We've seen delays up to 67ms on busy Linux and macOS machines,
/// and some other timeout failures even with a 150ms timeout.
pub const DEFAULT_MAX_REQUEST_DELAY: Duration = Duration::from_millis(300);

/// An internal type representing the item that's sent in the [`broadcast`] channel.
///
/// The actual type that matters is the [`ResponseSender`] but since there could be more than one
/// [`MockService`] verifying requests, the type must be wrapped so that it can be shared by all
/// receivers:
///
/// - The [`Arc`] makes sure the instance is on the heap, and can be shared properly between
///   threads and dropped when no longer needed.
/// - The [`Mutex`] ensures only one [`MockService`] instance can reply to the received request.
/// - The [`Option`] forces the [`MockService`] that handles the request to take ownership of it
///   because sending a response also forces the [`ResponseSender`] to be dropped.
type ProxyItem<Request, Response, Error> =
    Arc<Mutex<Option<ResponseSender<Request, Response, Error>>>>;

/// A service implementation that allows intercepting requests for checking them.
///
/// The type is generic over the request and response types, and also has an extra generic type
/// parameter that's used as a tag to determine if the internal assertions should panic or return
/// errors for proptest minimization. See [`AssertionType`] for more information.
///
/// The mock service can be cloned, and provides methods for checking the received requests as well
/// as responding to them individually.
///
/// Internally, the instance that's operating as the service will forward requests to a
/// [`broadcast`] channel that the other instances listen to.
///
/// See the [module-level documentation][`super::mock_service`] for an example.
pub struct MockService<Request, Response, Assertion, Error = BoxError> {
    receiver: broadcast::Receiver<ProxyItem<Request, Response, Error>>,
    sender: broadcast::Sender<ProxyItem<Request, Response, Error>>,
    max_request_delay: Duration,
    _assertion_type: PhantomData<Assertion>,
}

/// A builder type to create a [`MockService`].
///
/// Allows changing specific parameters used by the [`MockService`], if necessary. The default
/// parameters should be reasonable for most cases.
#[derive(Default)]
pub struct MockServiceBuilder {
    proxy_channel_size: Option<usize>,
    max_request_delay: Option<Duration>,
}

/// A helper type for responding to incoming requests.
///
/// An instance of this type is created for each request received by the [`MockService`]. It
/// contains the received request and a [`oneshot::Sender`] that can be used to respond to the
/// request.
///
/// If a response is not sent, the channel is closed and a [`BoxError`] is returned by the service
/// to the caller that sent the request.
#[must_use = "Tests may fail if a response is not sent back to the caller"]
pub struct ResponseSender<Request, Response, Error> {
    request: Request,
    response_sender: oneshot::Sender<Result<Response, Error>>,
}

/// The [`tower::Service`] implementation of the [`MockService`].
///
/// The [`MockService`] is always ready, and it intercepts the requests wrapping them in a
/// [`ResponseSender`] which can be used to send a response.
impl<Request, Response, Assertion, Error> Service<Request>
    for MockService<Request, Response, Assertion, Error>
where
    Response: Send + 'static,
    Error: Send + 'static,
{
    type Response = Response;
    type Error = Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let (response_sender, response_receiver) = ResponseSender::new(request);
        let proxy_item = Arc::new(Mutex::new(Some(response_sender)));

        let _ = self.sender.send(proxy_item);

        response_receiver
            .map(|response| {
                response.expect("A response was not sent by the `MockService` for a request")
            })
            .boxed()
    }
}

/// An entry point for starting the [`MockServiceBuilder`].
///
/// This `impl` block exists for ergonomic reasons. The generic type parameters don't matter,
/// because they are actually set by [`MockServiceBuilder::finish`].
impl MockService<(), (), ()> {
    /// Create a [`MockServiceBuilder`] to help with the creation of a [`MockService`].
    pub fn build() -> MockServiceBuilder {
        MockServiceBuilder::default()
    }
}

impl MockServiceBuilder {
    /// Configure the size of the proxy channel used for sending intercepted requests.
    ///
    /// This determines the maximum amount of requests that are kept in queue before the oldest
    /// request is dropped. This means that any tests that receive too many requests might ignore
    /// some requests if this parameter isn't properly configured.
    ///
    /// The default value of 100 should be enough for most cases.
    ///
    /// # Example
    ///
    /// ```
    /// # use zebra_test::mock_service::MockService;
    /// #
    /// let mock_service = MockService::build()
    ///     .with_proxy_channel_size(100)
    ///     .for_prop_tests();
    /// #
    /// # // Add types to satisfy the compiler's type inference.
    /// # let typed_mock_service: MockService<(), (), _> = mock_service;
    /// ```
    pub fn with_proxy_channel_size(mut self, size: usize) -> Self {
        self.proxy_channel_size = Some(size);
        self
    }

    /// Configure the time to wait for a request before considering no requests will be received.
    ///
    /// This determines the maximum amount of time that the [`MockService`] will wait for a request
    /// to be received before considering that a request will not be received.
    ///
    /// The default value of 25 ms should be enough for most cases.
    ///
    /// # Example
    ///
    /// ```
    /// # use std::time::Duration;
    /// #
    /// # use zebra_test::mock_service::MockService;
    /// #
    /// let mock_service = MockService::build()
    ///     .with_max_request_delay(Duration::from_millis(25))
    ///     .for_unit_tests();
    /// #
    /// # // Add types to satisfy the compiler's type inference.
    /// # let typed_mock_service: MockService<(), (), _> = mock_service;
    /// ```
    pub fn with_max_request_delay(mut self, max_request_delay: Duration) -> Self {
        self.max_request_delay = Some(max_request_delay);
        self
    }

    /// Create a [`MockService`] to be used in [`mod@proptest`]s.
    ///
    /// The assertions performed by [`MockService`] use the macros provided by [`mod@proptest`], like
    /// [`prop_assert`].
    pub fn for_prop_tests<Request, Response, Error>(
        self,
    ) -> MockService<Request, Response, PropTestAssertion, Error> {
        self.finish()
    }

    /// Create a [`MockService`] to be used in Rust unit tests.
    ///
    /// The assertions performed by [`MockService`] use the macros provided by default in Rust,
    /// like [`assert`].
    pub fn for_unit_tests<Request, Response, Error>(
        self,
    ) -> MockService<Request, Response, PanicAssertion, Error> {
        self.finish()
    }

    /// An internal helper method to create the actual [`MockService`].
    ///
    /// Note that this is used by both [`Self::for_prop_tests`] and [`Self::for_unit_tests`], the
    /// only difference being the `Assertion` generic type parameter, which Rust infers
    /// automatically.
    pub fn finish<Request, Response, Assertion, Error>(
        self,
    ) -> MockService<Request, Response, Assertion, Error> {
        let proxy_channel_size = self
            .proxy_channel_size
            .unwrap_or(DEFAULT_PROXY_CHANNEL_SIZE);
        let (sender, receiver) = broadcast::channel(proxy_channel_size);

        MockService {
            receiver,
            sender,
            max_request_delay: self.max_request_delay.unwrap_or(DEFAULT_MAX_REQUEST_DELAY),
            _assertion_type: PhantomData,
        }
    }
}

/// Implementation of [`MockService`] methods that use standard Rust panicking assertions.
impl<Request, Response, Error> MockService<Request, Response, PanicAssertion, Error> {
    /// Expect a specific request to be received.
    ///
    /// The expected request should be the next one in the internal queue, or if the queue is
    /// empty, it should be received in at most the max delay time configured by
    /// [`MockServiceBuilder::with_max_request_delay`].
    ///
    /// If the received request matches the expected request, a [`ResponseSender`] is returned
    /// which can be used to inspect the request and respond to it. If no response is sent, the
    /// sender of the requests receives an error.
    ///
    /// # Panics
    ///
    /// If no request is received or if a request is received that's not equal to the expected
    /// request, this method panics.
    ///
    /// # Example
    ///
    /// ```
    /// # use zebra_test::mock_service::MockService;
    /// # use tower::ServiceExt;
    /// #
    /// # let reactor = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("Failed to build Tokio runtime");
    /// #
    /// # reactor.block_on(async {
    /// #     let mut mock_service: MockService<_, _, _> = MockService::build().for_unit_tests();
    /// #     let mut service = mock_service.clone();
    /// #
    /// let call = tokio::spawn(mock_service.clone().oneshot("request"));
    ///
    /// mock_service.expect_request("request").await.respond("response");
    ///
    /// assert!(matches!(call.await, Ok(Ok("response"))));
    /// # });
    /// ```
    pub async fn expect_request(
        &mut self,
        expected: Request,
    ) -> ResponseSender<Request, Response, Error>
    where
        Request: PartialEq + Debug,
    {
        let response_sender = self.next_request().await;

        assert_eq!(
            response_sender.request,
            expected,
            "received an unexpected request\n \
             in {}",
            std::any::type_name::<Self>(),
        );

        response_sender
    }

    /// Expect a request to be received that matches a specified condition.
    ///
    /// There should be a request already in the internal queue, or a request should be received in
    /// at most the max delay time configured by [`MockServiceBuilder::with_max_request_delay`].
    ///
    /// The received request is passed to the `condition` function, which should return `true` if
    /// it matches the expected condition or `false` otherwise. If `true` is returned, a
    /// [`ResponseSender`] is returned which can be used to inspect the request again and respond
    /// to it. If no response is sent, the sender of the requests receives an error.
    ///
    /// # Panics
    ///
    /// If the `condition` function returns `false`, this method panics.
    ///
    /// # Example
    ///
    /// ```
    /// # use zebra_test::mock_service::MockService;
    /// # use tower::ServiceExt;
    /// #
    /// # let reactor = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("Failed to build Tokio runtime");
    /// #
    /// # reactor.block_on(async {
    /// #     let mut mock_service: MockService<_, _, _> = MockService::build().for_unit_tests();
    /// #     let mut service = mock_service.clone();
    /// #
    /// let call = tokio::spawn(mock_service.clone().oneshot(1));
    ///
    /// mock_service.expect_request_that(|request| *request > 0).await.respond("response");
    ///
    /// assert!(matches!(call.await, Ok(Ok("response"))));
    /// # });
    /// ```
    pub async fn expect_request_that(
        &mut self,
        condition: impl FnOnce(&Request) -> bool,
    ) -> ResponseSender<Request, Response, Error>
    where
        Request: Debug,
    {
        let response_sender = self.next_request().await;

        assert!(
            condition(&response_sender.request),
            "condition was false for request: {:?},\n \
             in {}",
            response_sender.request,
            std::any::type_name::<Self>(),
        );

        response_sender
    }

    /// Expect no requests to be received.
    ///
    /// The internal queue of received requests should be empty, and no new requests should arrive
    /// for the max delay time configured by [`MockServiceBuilder::with_max_request_delay`].
    ///
    /// # Panics
    ///
    /// If the queue is not empty or if a request is received before the max request delay timeout
    /// expires.
    ///
    /// # Example
    ///
    /// ```
    /// # use zebra_test::mock_service::MockService;
    /// # use tower::ServiceExt;
    /// #
    /// # let reactor = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("Failed to build Tokio runtime");
    /// #
    /// # reactor.block_on(async {
    /// #     let mut mock_service: MockService<(), (), _> = MockService::build().for_unit_tests();
    /// #
    /// mock_service.expect_no_requests().await;
    /// # });
    /// ```
    pub async fn expect_no_requests(&mut self)
    where
        Request: Debug,
    {
        if let Some(response_sender) = self.try_next_request().await {
            panic!(
                "received an unexpected request: {:?},\n \
                 in {}",
                response_sender.request,
                std::any::type_name::<Self>(),
            );
        }
    }

    /// Returns the next request from the queue,
    /// or panics if there are no requests after a short timeout.
    ///
    /// Returns the next request in the internal queue or waits at most the max delay time
    /// configured by [`MockServiceBuilder::with_max_request_delay`] for a new request to be
    /// received, and then returns that.
    ///
    /// # Panics
    ///
    /// If the queue is empty and a request is not received before the max request delay timeout
    /// expires.
    async fn next_request(&mut self) -> ResponseSender<Request, Response, Error> {
        match self.try_next_request().await {
            Some(request) => request,
            None => panic!(
                "timeout while waiting for a request\n \
                 in {}",
                std::any::type_name::<Self>(),
            ),
        }
    }
}

/// Implementation of [`MockService`] methods that use [`mod@proptest`] assertions.
impl<Request, Response, Error> MockService<Request, Response, PropTestAssertion, Error> {
    /// Expect a specific request to be received.
    ///
    /// The expected request should be the next one in the internal queue, or if the queue is
    /// empty, it should be received in at most the max delay time configured by
    /// [`MockServiceBuilder::with_max_request_delay`].
    ///
    /// If the received request matches the expected request, a [`ResponseSender`] is returned
    /// which can be used to inspect the request and respond to it. If no response is sent, the
    /// sender of the requests receives an error.
    ///
    /// If no request is received or if a request is received that's not equal to the expected
    /// request, this method returns an error generated by a [`mod@proptest`] assertion.
    ///
    /// # Example
    ///
    /// ```
    /// # use proptest::prelude::*;
    /// # use tower::ServiceExt;
    /// #
    /// # use zebra_test::mock_service::MockService;
    /// #
    /// # let reactor = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("Failed to build Tokio runtime");
    /// #
    /// # reactor.block_on(async {
    /// #     let test_code = || async {
    /// #         let mut mock_service: MockService<_, _, _> =
    /// #             MockService::build().for_prop_tests();
    /// #         let mut service = mock_service.clone();
    /// #
    /// let call = tokio::spawn(mock_service.clone().oneshot("request"));
    ///
    /// // NOTE: The try operator `?` is required for errors to be handled by proptest.
    /// mock_service
    ///     .expect_request("request").await?
    ///     .respond("response");
    ///
    /// prop_assert!(matches!(call.await, Ok(Ok("response"))));
    /// #
    /// #         Ok::<(), TestCaseError>(())
    /// #     };
    /// #     test_code().await
    /// # }).unwrap();
    /// ```
    pub async fn expect_request(
        &mut self,
        expected: Request,
    ) -> Result<ResponseSender<Request, Response, Error>, TestCaseError>
    where
        Request: PartialEq + Debug,
    {
        let response_sender = self.next_request().await?;

        prop_assert_eq!(
            &response_sender.request,
            &expected,
            "received an unexpected request\n \
             in {}",
            std::any::type_name::<Self>(),
        );

        Ok(response_sender)
    }

    /// Expect a request to be received that matches a specified condition.
    ///
    /// There should be a request already in the internal queue, or a request should be received in
    /// at most the max delay time configured by [`MockServiceBuilder::with_max_request_delay`].
    ///
    /// The received request is passed to the `condition` function, which should return `true` if
    /// it matches the expected condition or `false` otherwise. If `true` is returned, a
    /// [`ResponseSender`] is returned which can be used to inspect the request again and respond
    /// to it. If no response is sent, the sender of the requests receives an error.
    ///
    /// If the `condition` function returns `false`, this method returns an error generated by a
    /// [`mod@proptest`] assertion.
    ///
    /// # Example
    ///
    /// ```
    /// # use proptest::prelude::*;
    /// # use tower::ServiceExt;
    /// #
    /// # use zebra_test::mock_service::MockService;
    /// #
    /// # let reactor = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("Failed to build Tokio runtime");
    /// #
    /// # reactor.block_on(async {
    /// #     let test_code = || async {
    /// #         let mut mock_service: MockService<_, _, _> =
    /// #             MockService::build().for_prop_tests();
    /// #         let mut service = mock_service.clone();
    /// #
    /// let call = tokio::spawn(mock_service.clone().oneshot(1));
    ///
    /// // NOTE: The try operator `?` is required for errors to be handled by proptest.
    /// mock_service
    ///     .expect_request_that(|request| *request > 0).await?
    ///     .respond("OK");
    ///
    /// prop_assert!(matches!(call.await, Ok(Ok("OK"))));
    /// #
    /// #         Ok::<(), TestCaseError>(())
    /// #     };
    /// #     test_code().await
    /// # }).unwrap();
    /// ```
    pub async fn expect_request_that(
        &mut self,
        condition: impl FnOnce(&Request) -> bool,
    ) -> Result<ResponseSender<Request, Response, Error>, TestCaseError>
    where
        Request: Debug,
    {
        let response_sender = self.next_request().await?;

        prop_assert!(
            condition(&response_sender.request),
            "condition was false for request: {:?},\n \
             in {}",
            &response_sender.request,
            std::any::type_name::<Self>(),
        );

        Ok(response_sender)
    }

    /// Expect no requests to be received.
    ///
    /// The internal queue of received requests should be empty, and no new requests should arrive
    /// for the max delay time configured by [`MockServiceBuilder::with_max_request_delay`].
    ///
    /// If the queue is not empty or if a request is received before the max request delay timeout
    /// expires, an error generated by a [`mod@proptest`] assertion is returned.
    ///
    /// # Example
    ///
    /// ```
    /// # use proptest::prelude::TestCaseError;
    /// # use tower::ServiceExt;
    /// #
    /// # use zebra_test::mock_service::MockService;
    /// #
    /// # let reactor = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("Failed to build Tokio runtime");
    /// #
    /// # reactor.block_on(async {
    /// #     let test_code = || async {
    /// #         let mut mock_service: MockService<(), (), _> =
    /// #             MockService::build().for_prop_tests();
    /// #
    /// // NOTE: The try operator `?` is required for errors to be handled by proptest.
    /// mock_service.expect_no_requests().await?;
    /// #
    /// #         Ok::<(), TestCaseError>(())
    /// #     };
    /// #     test_code().await
    /// # }).unwrap();
    /// ```
    pub async fn expect_no_requests(&mut self) -> Result<(), TestCaseError>
    where
        Request: Debug,
    {
        match self.try_next_request().await {
            Some(response_sender) => {
                prop_assert!(
                    false,
                    "received an unexpected request: {:?},\n \
                     in {}",
                    response_sender.request,
                    std::any::type_name::<Self>(),
                );
                unreachable!("prop_assert!(false) returns an early error");
            }
            None => Ok(()),
        }
    }

    /// A helper method to get the next request from the queue.
    ///
    /// Returns the next request in the internal queue or waits at most the max delay time
    /// configured by [`MockServiceBuilder::with_max_request_delay`] for a new request to be
    /// received, and then returns that.
    ///
    /// If the queue is empty and a request is not received before the max request delay timeout
    /// expires, an error generated by a [`mod@proptest`] assertion is returned.
    async fn next_request(
        &mut self,
    ) -> Result<ResponseSender<Request, Response, Error>, TestCaseError> {
        match self.try_next_request().await {
            Some(request) => Ok(request),
            None => {
                prop_assert!(
                    false,
                    "timeout while waiting for a request\n \
                     in {}",
                    std::any::type_name::<Self>(),
                );
                unreachable!("prop_assert!(false) returns an early error");
            }
        }
    }
}

/// Code that is independent of the assertions used in [`MockService`].
impl<Request, Response, Assertion, Error> MockService<Request, Response, Assertion, Error> {
    /// Try to get the next request received.
    ///
    /// Returns the next element in the queue. If the queue is empty, waits at most the max request
    /// delay configured by [`MockServiceBuilder::with_max_request_delay`] for a request, and
    /// returns it.
    ///
    /// If no request is received, returns `None`.
    ///
    /// If too many requests are received and the queue fills up, the oldest requests are dropped
    /// and ignored. This means that calling this may not receive the next request if the queue is
    /// not dimensioned properly with the [`MockServiceBuilder::with_proxy_channel_size`] method.
    pub async fn try_next_request(&mut self) -> Option<ResponseSender<Request, Response, Error>> {
        loop {
            match timeout(self.max_request_delay, self.receiver.recv()).await {
                Ok(Ok(item)) => {
                    if let Some(proxy_item) = item.lock().await.take() {
                        return Some(proxy_item);
                    }
                }
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Err(RecvError::Closed)) => unreachable!("sender is never closed"),
                Err(_timeout) => return None,
            }
        }
    }
}

impl<Request, Response, Assertion, Error> Clone
    for MockService<Request, Response, Assertion, Error>
{
    /// Clones the [`MockService`].
    ///
    /// This is a cheap operation, because it simply clones the [`broadcast`] channel endpoints.
    fn clone(&self) -> Self {
        MockService {
            receiver: self.sender.subscribe(),
            sender: self.sender.clone(),
            max_request_delay: self.max_request_delay,
            _assertion_type: PhantomData,
        }
    }
}

impl<Request, Response, Error> ResponseSender<Request, Response, Error> {
    /// Create a [`ResponseSender`] for a given `request`.
    fn new(request: Request) -> (Self, oneshot::Receiver<Result<Response, Error>>) {
        let (response_sender, response_receiver) = oneshot::channel();

        (
            ResponseSender {
                request,
                response_sender,
            },
            response_receiver,
        )
    }

    /// Access the `request` that's awaiting a response.
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Respond to the request using a fixed response value.
    ///
    /// The `response` can be of the `Response` type or a [`Result`]. This allows sending an error
    /// representing an error while processing the request.
    ///
    /// This method takes ownership of the [`ResponseSender`] so that only one response can be
    /// sent.
    ///
    /// If `respond` or `respond_with` are not called, the caller will panic.
    ///
    /// # Example
    ///
    /// ```
    /// # use zebra_test::mock_service::MockService;
    /// # use tower::{Service, ServiceExt};
    /// #
    /// # let reactor = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("Failed to build Tokio runtime");
    /// #
    /// # reactor.block_on(async {
    /// // Mock a service with a `String` as the service `Error` type.
    /// let mut mock_service: MockService<_, _, _, String> =
    ///     MockService::build().for_unit_tests();
    ///
    /// # let mut service = mock_service.clone();
    /// # let task = tokio::spawn(async move {
    /// #     let first_call_result = (&mut service).oneshot(1).await;
    /// #     let second_call_result = service.oneshot(1).await;
    /// #
    /// #     (first_call_result, second_call_result)
    /// # });
    /// #
    /// mock_service
    ///     .expect_request(1)
    ///     .await
    ///     .respond("Received one".to_owned());
    ///
    /// mock_service
    ///     .expect_request(1)
    ///     .await
    ///     .respond(Err("Duplicate request"));
    /// # });
    /// ```
    pub fn respond(self, response: impl ResponseResult<Response, Error>) {
        let _ = self.response_sender.send(response.into_result());
    }

    /// Respond to the request by calculating a value from the request.
    ///
    /// The response can be of the `Response` type or a [`Result`]. This allows sending an error
    /// representing an error while processing the request.
    ///
    /// This method takes ownership of the [`ResponseSender`] so that only one response can be
    /// sent.
    ///
    /// If `respond` or `respond_with` are not called, the caller will panic.
    ///
    /// # Example
    ///
    /// ```
    /// # use zebra_test::mock_service::MockService;
    /// # use tower::{Service, ServiceExt};
    /// #
    /// # let reactor = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("Failed to build Tokio runtime");
    /// #
    /// # reactor.block_on(async {
    /// // Mock a service with a `String` as the service `Error` type.
    /// let mut mock_service: MockService<_, _, _, String> =
    ///     MockService::build().for_unit_tests();
    ///
    /// # let mut service = mock_service.clone();
    /// # let task = tokio::spawn(async move {
    /// #     let first_call_result = (&mut service).oneshot(1).await;
    /// #     let second_call_result = service.oneshot(1).await;
    /// #
    /// #     (first_call_result, second_call_result)
    /// # });
    /// #
    /// mock_service
    ///     .expect_request(1)
    ///     .await
    ///     .respond_with(|req| format!("Received: {}", req));
    ///
    /// mock_service
    ///     .expect_request(1)
    ///     .await
    ///     .respond_with(|req| Err(format!("Duplicate request: {}", req)));
    /// # });
    /// ```
    pub fn respond_with<F, R>(self, response_fn: F)
    where
        F: FnOnce(&Request) -> R,
        R: ResponseResult<Response, Error>,
    {
        let response_result = response_fn(self.request()).into_result();
        let _ = self.response_sender.send(response_result);
    }
}

/// A representation of an assertion type.
///
/// This trait is used to group the types of assertions that the [`MockService`] can do. There are
/// currently two types that are used as type-system tags on the [`MockService`]:
///
/// - [`PanicAssertion`]
/// - [`PropTestAssertion`]
trait AssertionType {}

/// Represents normal Rust assertions that panic, like [`assert_eq`].
pub enum PanicAssertion {}

/// Represents [`mod@proptest`] assertions that return errors, like [`prop_assert_eq`].
pub enum PropTestAssertion {}

impl AssertionType for PanicAssertion {}

impl AssertionType for PropTestAssertion {}

/// A helper trait to improve ergonomics when sending a response.
///
/// This allows the [`ResponseSender::respond`] method to receive either a [`Result`] or just the
/// response type, which it automatically wraps in an `Ok` variant.
pub trait ResponseResult<Response, Error> {
    /// Converts the type into a [`Result`] that can be sent as a response.
    fn into_result(self) -> Result<Response, Error>;
}

impl<Response, Error> ResponseResult<Response, Error> for Response {
    fn into_result(self) -> Result<Response, Error> {
        Ok(self)
    }
}

impl<Response, SourceError, TargetError> ResponseResult<Response, TargetError>
    for Result<Response, SourceError>
where
    SourceError: Into<TargetError>,
{
    fn into_result(self) -> Result<Response, TargetError> {
        self.map_err(|source_error| source_error.into())
    }
}
