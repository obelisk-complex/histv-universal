//! GUI implementation of `BatchControl` — wraps the existing
//! `Mutex<BatchState>` and uses `AppHandle` emit-and-poll for prompts.
//!
//! Uses `tokio::task::block_in_place` to safely call async mutex operations
//! from synchronous `BatchControl` trait methods within a tokio runtime.

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::events::BatchControl;

/// GUI batch control, wrapping the existing shared state and Tauri handle.
pub struct GuiBatchControl {
    state: Arc<crate::AppState>,
    app: AppHandle,
}

impl GuiBatchControl {
    pub fn new(state: Arc<crate::AppState>, app: AppHandle) -> Self {
        Self { state, app }
    }

    /// Run an async block from a sync context within a tokio runtime.
    /// Uses block_in_place to avoid deadlocking the runtime.
    fn block_on<F, T>(&self, f: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(f)
        })
    }
}

impl BatchControl for GuiBatchControl {
    fn should_cancel_current(&self) -> bool {
        self.block_on(async {
            let b = self.state.batch.lock().await;
            b.cancel_current
        })
    }

    fn should_cancel_all(&self) -> bool {
        self.block_on(async {
            let b = self.state.batch.lock().await;
            b.cancel_all
        })
    }

    fn is_paused(&self) -> bool {
        self.block_on(async {
            let b = self.state.batch.lock().await;
            b.paused
        })
    }

    fn clear_cancel_current(&self) {
        self.block_on(async {
            let mut b = self.state.batch.lock().await;
            b.cancel_current = false;
        });
    }

    fn overwrite_always(&self) -> bool {
        self.block_on(async {
            let b = self.state.batch.lock().await;
            b.overwrite_always
        })
    }

    fn set_overwrite_always(&self) {
        self.block_on(async {
            let mut b = self.state.batch.lock().await;
            b.overwrite_always = true;
        });
    }

    fn overwrite_prompt(&self, path: &str) -> String {
        let _ = self.app.emit("overwrite-prompt", path);

        self.block_on(async {
            loop {
                {
                    let mut b = self.state.batch.lock().await;
                    if let Some(response) = b.overwrite_response.take() {
                        return response;
                    }
                    if b.cancel_all {
                        return "cancel".to_string();
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
    }

    fn hw_fallback_offered(&self) -> bool {
        self.block_on(async {
            let b = self.state.batch.lock().await;
            b.hw_fallback_offered
        })
    }

    fn set_hw_fallback_offered(&self) {
        self.block_on(async {
            let mut b = self.state.batch.lock().await;
            b.hw_fallback_offered = true;
        });
    }

    fn fallback_prompt(&self, filename: &str) -> String {
        let _ = self.app.emit("fallback-prompt", filename);

        self.block_on(async {
            loop {
                {
                    let mut b = self.state.batch.lock().await;
                    if let Some(response) = b.fallback_response.take() {
                        return response;
                    }
                    if b.cancel_all {
                        return "no".to_string();
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
    }
}
