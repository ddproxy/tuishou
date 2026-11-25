#![forbid(unsafe_code)]

pub mod archive;
pub mod auto_update;
pub mod cli;
pub mod completions;
pub mod component;
pub mod config;
pub mod docs_static;
pub mod download;
pub mod duration;
pub mod error;
pub mod fleet;
pub mod http;
pub mod install;
pub mod logging;
pub mod lu;
pub mod process;
pub mod release;
pub mod report;
pub mod rollback;
pub mod self_update;
pub mod state;
pub mod status;
pub mod target;
#[cfg(feature = "top")]
pub mod top;
pub mod verify;
pub mod version;

/// Serializes tests that mutate process-wide environment variables.
#[cfg(test)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
