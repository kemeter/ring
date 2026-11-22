//! Image pulls run in the background, so the scheduler never waits on one.
//!
//! The scheduler reconciles deployments one after the other. A pull that
//! hangs (a registry that accepts the connection and answers slowly, or not
//! at all) used to hold the whole pass until the daemon gave up: on a small
//! host every other deployment went unreconciled for minutes, crashed
//! instances included. A start that needs a pull now starts it here and
//! returns at once; a later pass picks up its result.
//!
//! One registry per runtime: Docker and Podman keep separate image stores, so
//! a pull done by one says nothing about the other.

use crate::hypervisor::error::RuntimeError;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::warn;

/// Longest a background pull may run before it is reported as failed, so a
/// registry that never answers cannot keep a deployment waiting forever.
pub(crate) const PULL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long a start waits for a pull before handing it to a later pass. Most
/// pulls (a cached tag checked against its registry) end well within it, so
/// they start in the same pass; only a slow one is deferred.
pub(crate) const INLINE_WAIT: Duration = Duration::from_secs(2);

/// What a pull gives back: the image digest, when the daemon reports one.
pub(crate) type PullResult = Result<Option<String>, RuntimeError>;

enum PullState {
    /// Running; the receiver turns true when it ends.
    InFlight(tokio::sync::watch::Receiver<bool>),
    Done(PullResult),
}

/// Where a start stands with the pull of its image.
pub(crate) enum PullProgress {
    /// The pull just started: say so once.
    Started,
    /// Still running.
    InFlight,
    /// Finished; its result is handed over once.
    Done(PullResult),
}

#[derive(Clone, Default)]
pub(crate) struct ImagePulls {
    inner: Arc<Mutex<HashMap<String, PullState>>>,
}

impl ImagePulls {
    /// Hand over the result of `image`'s pull if it finished, or start it with
    /// `pull` if none is running.
    pub(crate) fn take_or_start<F>(&self, image: &str, pull: F) -> PullProgress
    where
        F: FnOnce() -> BoxFuture<'static, PullResult>,
    {
        self.take_or_start_within(image, PULL_TIMEOUT, pull)
    }

    /// Like [`take_or_start`](Self::take_or_start), but waits up to `wait`
    /// for the pull to end, so a quick one is handed over in the same call.
    pub(crate) async fn take_or_start_waiting<F>(
        &self,
        image: &str,
        wait: Duration,
        pull: F,
    ) -> PullProgress
    where
        F: FnOnce() -> BoxFuture<'static, PullResult>,
    {
        let first = self.take_or_start(image, pull);
        if matches!(first, PullProgress::Done(_)) {
            return first;
        }
        if let Some(mut ended) = self.receiver(image) {
            let _ = tokio::time::timeout(wait, ended.wait_for(|done| *done)).await;
        }
        match self.take_or_start(image, || unreachable!("the pull is already known")) {
            PullProgress::Done(result) => PullProgress::Done(result),
            _ => first,
        }
    }

    fn receiver(&self, image: &str) -> Option<tokio::sync::watch::Receiver<bool>> {
        match self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(image)
        {
            Some(PullState::InFlight(rx)) => Some(rx.clone()),
            _ => None,
        }
    }

    fn take_or_start_within<F>(&self, image: &str, timeout: Duration, pull: F) -> PullProgress
    where
        F: FnOnce() -> BoxFuture<'static, PullResult>,
    {
        let mut pulls = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match pulls.remove(image) {
            Some(PullState::Done(result)) => return PullProgress::Done(result),
            Some(PullState::InFlight(rx)) => {
                pulls.insert(image.to_string(), PullState::InFlight(rx));
                return PullProgress::InFlight;
            }
            None => {}
        }
        let (ended, rx) = tokio::sync::watch::channel(false);
        pulls.insert(image.to_string(), PullState::InFlight(rx));
        drop(pulls);

        let inner = self.inner.clone();
        let image = image.to_string();
        let future = pull();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(timeout, future).await {
                Ok(result) => result,
                Err(_) => {
                    warn!(
                        "pull of image '{}' did not finish within {}s",
                        image,
                        timeout.as_secs()
                    );
                    Err(RuntimeError::ImagePullFailed(format!(
                        "the pull of '{}' did not finish within {}s; is the registry answering?",
                        image,
                        timeout.as_secs()
                    )))
                }
            };
            inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(image, PullState::Done(result));
            let _ = ended.send(true);
        });
        PullProgress::Started
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_pull_runs_once_and_its_result_is_handed_over_once() {
        let pulls = ImagePulls::default();
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let mut gate = Some(gate);
        let mut start = || {
            let gate = gate.take().expect("the pull is started only once");
            async move {
                gate.await.unwrap();
                Ok(Some("sha256:abc".to_string()))
            }
            .boxed()
        };

        assert!(matches!(
            pulls.take_or_start("app:1", &mut start),
            PullProgress::Started
        ));
        // A pass while it runs does not wait, and does not start another.
        assert!(matches!(
            pulls.take_or_start("app:1", &mut start),
            PullProgress::InFlight
        ));

        release.send(()).unwrap();
        settle().await;
        match pulls.take_or_start("app:1", &mut start) {
            PullProgress::Done(Ok(digest)) => assert_eq!(digest.as_deref(), Some("sha256:abc")),
            _ => panic!("the finished pull should be handed over"),
        }
    }

    #[tokio::test]
    async fn a_failed_pull_hands_its_error_over() {
        let pulls = ImagePulls::default();
        pulls.take_or_start("bad:1", || {
            async { Err(RuntimeError::ImageNotFound("no such tag".into())) }.boxed()
        });
        settle().await;
        assert!(matches!(
            pulls.take_or_start("bad:1", || unreachable!()),
            PullProgress::Done(Err(RuntimeError::ImageNotFound(_)))
        ));
    }

    #[tokio::test]
    async fn a_pull_that_never_ends_is_reported_as_failed() {
        let pulls = ImagePulls::default();
        pulls.take_or_start_within("hang:1", Duration::from_millis(20), || {
            futures::future::pending().boxed()
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
        match pulls.take_or_start("hang:1", || unreachable!()) {
            PullProgress::Done(Err(RuntimeError::ImagePullFailed(msg))) => {
                assert!(msg.contains("did not finish"), "{msg}")
            }
            _ => panic!("a hung pull must end as a failure"),
        }
    }

    #[tokio::test]
    async fn images_are_pulled_independently() {
        let pulls = ImagePulls::default();
        pulls.take_or_start("a:1", || futures::future::pending().boxed());
        assert!(matches!(
            pulls.take_or_start("b:1", || async { Ok(None) }.boxed()),
            PullProgress::Started
        ));
    }

    #[tokio::test]
    async fn a_quick_pull_is_handed_over_in_the_same_call() {
        let pulls = ImagePulls::default();
        let progress = pulls
            .take_or_start_waiting("quick:1", Duration::from_secs(5), || {
                async { Ok(Some("sha256:q".to_string())) }.boxed()
            })
            .await;
        assert!(matches!(progress, PullProgress::Done(Ok(Some(_)))));
    }

    #[tokio::test]
    async fn a_slow_pull_is_left_to_a_later_pass() {
        let pulls = ImagePulls::default();
        let started = std::time::Instant::now();
        let progress = pulls
            .take_or_start_waiting("slow:1", Duration::from_millis(50), || {
                futures::future::pending().boxed()
            })
            .await;
        assert!(matches!(progress, PullProgress::Started));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
