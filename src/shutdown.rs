use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::watch;

/// Coordinates graceful shutdown across multiple async tasks.
///
/// Late subscribers (created after `shutdown()`) will see the shutdown
/// immediately. Dropping the coordinator also wakes all receivers.
#[derive(Clone)]
pub struct ShutdownCoordinator {
    sender: Arc<watch::Sender<bool>>,
    is_shutting_down: Arc<AtomicBool>,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self {
            sender: Arc::new(sender),
            is_shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn subscribe(&self) -> ShutdownReceiver {
        ShutdownReceiver {
            receiver: self.sender.subscribe(),
        }
    }

    /// Trigger shutdown. Safe to call multiple times — only the first call fires.
    pub fn shutdown(&self) {
        if !self.is_shutting_down.swap(true, Ordering::AcqRel) {
            self.sender.send_replace(true);
        }
    }
}

pub struct ShutdownReceiver {
    receiver: watch::Receiver<bool>,
}

impl ShutdownReceiver {
    pub async fn recv(&mut self) {
        let _ = self.receiver.wait_for(|&shutdown| shutdown).await;
    }
}
