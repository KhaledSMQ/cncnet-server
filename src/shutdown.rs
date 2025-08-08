use tokio::sync::broadcast;
use tokio::sync::RwLock;
use std::sync::Arc;

#[derive(Clone)]
pub struct ShutdownCoordinator {
    sender: broadcast::Sender<()>,
    is_shutting_down: Arc<RwLock<bool>>,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1);
        Self {
            sender,
            is_shutting_down: Arc::new(RwLock::new(false)),
        }
    }

    pub fn subscribe(&self) -> ShutdownReceiver {
        ShutdownReceiver {
            receiver: self.sender.subscribe(),
        }
    }

    pub async fn shutdown(&self) {
        let mut shutting_down = self.is_shutting_down.write().await;
        if !*shutting_down {
            *shutting_down = true;
            let _ = self.sender.send(());
        }
    }

    pub async fn is_shutting_down(&self) -> bool {
        *self.is_shutting_down.read().await
    }
}

pub struct ShutdownReceiver {
    receiver: broadcast::Receiver<()>,
}

impl ShutdownReceiver {
    pub async fn recv(&mut self) {
        let _ = self.receiver.recv().await;
    }
}