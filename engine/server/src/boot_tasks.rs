//! Ownership of the process-lifetime background tasks spawned at boot
//! (WI-4650). They used to be detached `tokio::spawn`s: a panic was invisible
//! and shutdown could not stop or wait for them. They now live in a `JoinSet`
//! on `AppState`, each wrapped so a cancel token ends it at its next await, and
//! `shutdown` cancels and awaits them with a timeout.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct BootTasks {
    cancel: CancellationToken,
    set: Mutex<JoinSet<()>>,
}

impl BootTasks {
    /// Run `fut` until it finishes or `shutdown` cancels it. `name` is only
    /// used to log a panic.
    pub fn spawn<F>(&self, name: &'static str, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = self.cancel.clone();
        let mut set = self.set.lock().unwrap_or_else(|e| e.into_inner());
        // Reap finished tasks so a long-lived set does not hold their results,
        // and report any that panicked.
        while let Some(done) = set.try_join_next() {
            if let Err(e) = done {
                if e.is_panic() {
                    tracing::error!(error = %e, "a boot task panicked");
                }
            }
        }
        set.spawn(async move {
            let run = tokio::spawn(fut);
            let abort = run.abort_handle();
            tokio::select! {
                res = run => {
                    if let Err(e) = res {
                        if e.is_panic() {
                            tracing::error!(task = name, "boot task panicked");
                        }
                    }
                }
                _ = cancel.cancelled() => abort.abort(),
            }
        });
    }

    /// Cancel every boot task and wait for them, at most `timeout`. Returns the
    /// number still running when the wait gave up.
    pub async fn shutdown(&self, timeout: Duration) -> usize {
        self.cancel.cancel();
        let mut set = {
            let mut guard = self.set.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *guard)
        };
        let drained =
            tokio::time::timeout(timeout, async { while set.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            let left = set.len();
            tracing::warn!(left, "boot tasks did not stop in time; aborting");
            set.abort_all();
            return left;
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct Flag(Arc<AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn shutdown_cancels_and_awaits_the_boot_tasks() {
        let tasks = BootTasks::default();
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = Flag(Arc::clone(&dropped));
        tasks.spawn("forever", async move {
            let _flag = flag;
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        tasks.spawn("panics", async { panic!("boom") });
        tokio::task::yield_now().await;
        let left = tasks.shutdown(Duration::from_secs(5)).await;
        assert_eq!(left, 0);
        // The task's future was dropped, so it is gone, not merely detached.
        assert!(dropped.load(Ordering::SeqCst));
    }
}
