use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::engine::Engine;
use crate::task::Progress;

/// FIFO queue that runs downloads with bounded concurrency.
///
/// v0.1: in-memory. The daemon (P1) will persist the queue and expose
/// add/status/pause/resume over a local RPC channel.
pub struct Queue {
    engine: Arc<Engine>,
    sem: Arc<Semaphore>,
    root: CancellationToken,
}

impl Queue {
    pub fn new(engine: Arc<Engine>, max_concurrent: usize) -> Self {
        Self {
            engine,
            sem: Arc::new(Semaphore::new(max_concurrent)),
            root: CancellationToken::new(),
        }
    }

    /// Cancels everything currently queued or running.
    pub fn token(&self) -> CancellationToken {
        self.root.clone()
    }

    /// Enqueue a URL; it runs as soon as a slot frees up.
    /// TODO(P1): persist queued tasks so a restart does not lose them.
    pub fn push(
        &self,
        url: String,
        dest_dir: Option<PathBuf>,
        on_progress: impl FnMut(Progress) + Send + 'static,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let engine = self.engine.clone();
        let sem = self.sem.clone();
        let token = self.root.child_token();
        let task_id = id.clone();

        tokio::spawn(async move {
            let _permit = sem.acquire().await;
            if let Err(e) = engine.download(&url, dest_dir, token, on_progress).await {
                tracing::warn!(task = %task_id, error = %e, "download failed");
            }
        });

        id
    }
}
