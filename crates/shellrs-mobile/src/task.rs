//! Work on the core's runtime, tied to the call that started it.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::failure::SshError;

/// What a call fails with when its work never finished.
pub(crate) trait CallError: Send + 'static {
    /// The core is gone, and its runtime with it.
    fn closed() -> Self;
    /// The work panicked: a bug in ShellRS.
    fn internal(detail: String) -> Self;
}

impl CallError for SshError {
    fn closed() -> Self {
        Self::Closed
    }

    fn internal(detail: String) -> Self {
        Self::Internal { detail }
    }
}

/// Run `future` on `runtime` and wait for it. Swift cancelling its task or
/// Kotlin its coroutine drops the call's future, which stops the work too
/// rather than leaving it running unseen. A panic comes back as an error.
pub(crate) async fn run<T: Send + 'static, E: CallError>(
    runtime: &Handle,
    future: impl Future<Output = Result<T, E>> + Send + 'static,
) -> Result<T, E> {
    match AbortOnDrop(runtime.spawn(future)).await {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Err(E::closed()),
        Err(panic) => Err(E::internal(panic.to_string())),
    }
}

/// Run `work`, which blocks, on `runtime`'s threads for it, and wait for
/// it. A panic goes on in the caller, to come back from [`run`].
pub(crate) async fn blocking<R: Send + 'static>(
    runtime: &Handle,
    work: impl FnOnce() -> R + Send + 'static,
) -> R {
    match runtime.spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        // The runtime is going, and the caller's task with it.
        Err(_) => std::future::pending().await,
    }
}

/// A task that ends when its handle is dropped.
pub(crate) struct AbortOnDrop<T>(pub(crate) JoinHandle<T>);

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use crate::failure::SshError;

    /// Dropped when the task's future is.
    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// The app giving up on a call stops the work it started.
    #[test]
    fn a_call_given_up_on_stops_its_work() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let work = {
            let guard = Dropped(dropped.clone());
            async move {
                let _guard = guard;
                std::future::pending::<()>().await;
                Ok::<_, SshError>(())
            }
        };
        // Started, then dropped unfinished, as a cancelled Swift task does.
        let call = super::run(runtime.handle(), work);
        assert!(futures::FutureExt::now_or_never(call).is_none());
        for _ in 0..100 {
            if dropped.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the work kept running");
    }
}
