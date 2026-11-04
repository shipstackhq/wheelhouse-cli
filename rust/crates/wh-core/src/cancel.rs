//! A stop signal one task can raise and many can wait on: the Rust side of Node's `AbortSignal`.

use std::sync::Arc;
use tokio::sync::watch;

#[derive(Clone)]
pub struct Cancel {
    tx: Arc<watch::Sender<bool>>,
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

impl Cancel {
    pub fn new() -> Self {
        Cancel {
            tx: Arc::new(watch::channel(false).0),
        }
    }

    pub fn cancel(&self) {
        self.tx.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolves once cancelled (at once when it already is).
    pub async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waiters_wake_when_cancelled_and_late_ones_return_at_once() {
        let c = Cancel::new();
        assert!(!c.is_cancelled());
        let c2 = c.clone();
        let h = tokio::spawn(async move { c2.cancelled().await });
        tokio::task::yield_now().await;
        c.cancel();
        h.await.unwrap();
        c.cancelled().await;
        assert!(c.is_cancelled());
    }
}
