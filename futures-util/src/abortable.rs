use crate::task::AtomicWaker;
use alloc::sync::Arc;
use core::fmt;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use futures_core::future::Future;
use futures_core::task::{Context, Poll};
use futures_core::Stream;
use pin_project_lite::pin_project;

pin_project! {
    /// A future/stream which can be remotely short-circuited using an `AbortHandle`.
    #[derive(Debug, Clone)]
    #[must_use = "futures/streams do nothing unless you poll them"]
    pub struct Abortable<T> {
        #[pin]
        task: T,
        inner: Arc<AbortInner>,
    }
}

// 代表某个东西是可终止的
impl<T> Abortable<T> {
    /// Creates a new `Abortable` future/stream using an existing `AbortRegistration`.
    /// `AbortRegistration`s can be acquired through `AbortHandle::new`.
    ///
    /// When `abort` is called on the handle tied to `reg` or if `abort` has
    /// already been called, the future/stream will complete immediately without making
    /// any further progress.
    ///
    /// # Examples:
    ///
    /// Usage with futures:
    ///
    /// ```
    /// # futures::executor::block_on(async {
    /// use futures::future::{Abortable, AbortHandle, Aborted};
    ///
    /// let (abort_handle, abort_registration) = AbortHandle::new_pair();
    /// let future = Abortable::new(async { 2 }, abort_registration);
    /// abort_handle.abort();
    /// assert_eq!(future.await, Err(Aborted));
    /// # });
    /// ```
    ///
    /// Usage with streams:
    ///
    /// ```
    /// # futures::executor::block_on(async {
    /// # use futures::future::{Abortable, AbortHandle};
    /// # use futures::stream::{self, StreamExt};
    ///
    /// let (abort_handle, abort_registration) = AbortHandle::new_pair();
    /// let mut stream = Abortable::new(stream::iter(vec![1, 2, 3]), abort_registration);
    /// abort_handle.abort();
    /// assert_eq!(stream.next().await, None);
    /// # });
    /// ```
    pub fn new(task: T, reg: AbortRegistration) -> Self {
        Self { task, inner: reg.inner }
    }

    /// Checks whether the task has been aborted. Note that all this
    /// method indicates is whether [`AbortHandle::abort`] was *called*.
    /// This means that it will return `true` even if:
    /// * `abort` was called after the task had completed.
    /// * `abort` was called while the task was being polled - the task may still be running and
    /// will not be stopped until `poll` returns.
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::Relaxed)
    }
}

/// A registration handle for an `Abortable` task.
/// Values of this type can be acquired from `AbortHandle::new` and are used
/// in calls to `Abortable::new`.
#[derive(Debug)]
pub struct AbortRegistration {
    // 内部包含一个waker对象
    pub(crate) inner: Arc<AbortInner>,
}

impl AbortRegistration {
    /// Create an [`AbortHandle`] from the given [`AbortRegistration`].
    ///
    /// The created [`AbortHandle`] is functionally the same as any other
    /// [`AbortHandle`]s that are associated with the same [`AbortRegistration`],
    /// such as the one created by [`AbortHandle::new_pair`].
    /// 因为handle和本对象内的inner是一样的  所以可以快速复制handle
    pub fn handle(&self) -> AbortHandle {
        AbortHandle { inner: self.inner.clone() }
    }
}

/// A handle to an `Abortable` task.
#[derive(Debug, Clone)]
pub struct AbortHandle {
    inner: Arc<AbortInner>,
}

impl AbortHandle {
    /// Creates an (`AbortHandle`, `AbortRegistration`) pair which can be used
    /// to abort a running future or stream.
    ///
    /// This function is usually paired with a call to [`Abortable::new`].
    /// 首先第一步调用该方法
    pub fn new_pair() -> (Self, AbortRegistration) {
        // 主要就是包裹了这个waker对象
        let inner =
            Arc::new(AbortInner { waker: AtomicWaker::new(), aborted: AtomicBool::new(false) });

        (Self { inner: inner.clone() }, AbortRegistration { inner })
    }
}

// Inner type storing the waker to awaken and a bool indicating that it
// should be aborted.
// 代表一个可终止对象
#[derive(Debug)]
pub(crate) struct AbortInner {
    // waker对象可以设置
    pub(crate) waker: AtomicWaker,
    // 是否已经触发了终止
    pub(crate) aborted: AtomicBool,
}

/// Indicator that the `Abortable` task was aborted.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Aborted;

impl fmt::Display for Aborted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`Abortable` future has been aborted")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Aborted {}


// 此时T已经被包装成一个可终止对象了
impl<T> Abortable<T> {

    // 这里尝试获取T T可以是任何对象
    fn try_poll<I>(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        poll: impl Fn(Pin<&mut T>, &mut Context<'_>) -> Poll<I>,
    ) -> Poll<Result<I, Aborted>> {
        // Check if the task has been aborted
        // 已经被终止了 直接返回
        if self.is_aborted() {
            return Poll::Ready(Err(Aborted));
        }

        // attempt to complete the task
        if let Poll::Ready(x) = poll(self.as_mut().project().task, cx) {
            return Poll::Ready(Ok(x));
        }

        // Register to receive a wakeup if the task is aborted in the future
        // 此时没有直接获取到结果 要注册一个waker 是把上游的waker注册进来  在abort时会唤醒 同时应该会再次进入该方法 并发现is_aborted为true 并返回err
        self.inner.waker.register(cx.waker());

        // Check to see if the task was aborted between the first check and
        // registration.
        // Checking with `is_aborted` which uses `Relaxed` is sufficient because
        // `register` introduces an `AcqRel` barrier.
        if self.is_aborted() {
            return Poll::Ready(Err(Aborted));
        }

        Poll::Pending
    }
}

// 当内部元素是future时 可以把abortable当作future来用
impl<Fut> Future for Abortable<Fut>
where
    Fut: Future,
{
    type Output = Result<Fut::Output, Aborted>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.try_poll(cx, |fut, cx| fut.poll(cx))
    }
}

impl<St> Stream for Abortable<St>
where
    St: Stream,
{
    type Item = St::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.try_poll(cx, |stream, cx| stream.poll_next(cx)).map(Result::ok).map(Option::flatten)
    }
}

// 作为handle提供的方法
impl AbortHandle {
    /// Abort the `Abortable` stream/future associated with this handle.
    ///
    /// Notifies the Abortable task associated with this handle that it
    /// should abort. Note that if the task is currently being polled on
    /// another thread, it will not immediately stop running. Instead, it will
    /// continue to run until its poll method returns.
    /// 触发终止 同时唤醒某个对象  也就是说终止被认为是调用了wake
    pub fn abort(&self) {
        self.inner.aborted.store(true, Ordering::Relaxed);
        self.inner.waker.wake();
    }

    /// Checks whether [`AbortHandle::abort`] was *called* on any associated
    /// [`AbortHandle`]s, which includes all the [`AbortHandle`]s linked with
    /// the same [`AbortRegistration`]. This means that it will return `true`
    /// even if:
    /// * `abort` was called after the task had completed.
    /// * `abort` was called while the task was being polled - the task may still be running and
    /// will not be stopped until `poll` returns.
    ///
    /// This operation has a Relaxed ordering.
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::Relaxed)
    }
}
