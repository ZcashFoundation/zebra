use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project::pin_project;

/// A helper [`Future`] wrapper that will always return [`Poll::Ready`].
///
/// If the inner [`Future`] `F` is ready and produces an output `value`, then [`NowOrNever`] will
/// also be ready but with an output `Some(value)`.
///
/// If the inner [`Future`] `F` is not ready, then:
///
/// - [`NowOrNever`] will be still be ready but with an output `None`,
/// - and the task associated with the future will be scheduled to awake whenever the inner
///   [`Future`] `F` becomes ready.
///
/// This is different from [`FutureExt::now_or_never`] because `now_or_never` uses a fake task
/// [`Context`], which means that calling `now_or_never` inside an `async` function doesn't
/// schedule the generated future to be polled again when the inner future becomes ready.
///
/// # Examples
///
/// ```
/// use futures::{FutureExt, future};
/// # use zebrad::async_ext::NowOrLater;
///
/// let inner_future = future::ready(());
///
/// # let runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
/// #
/// # runtime.block_on(async move {
/// assert_eq!(NowOrLater(inner_future).await, Some(()));
/// # });
/// ```
///
/// ```
/// use futures::{FutureExt, future};
/// # use zebrad::async_ext::NowOrLater;
///
/// let inner_future = future::pending::<()>();
///
/// # let runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
/// #
/// # runtime.block_on(async move {
/// assert_eq!(NowOrLater(inner_future).await, None);
/// # });
/// ```
#[pin_project]
pub struct NowOrLater<F>(#[pin] pub F);

impl<F: Future> Future for NowOrLater<F> {
    type Output = Option<F::Output>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().0.poll(context) {
            Poll::Ready(value) => Poll::Ready(Some(value)),
            Poll::Pending => Poll::Ready(None),
        }
    }
}
