#![forbid(unsafe_code)]

mod archive;
mod auto_update;
mod cli;
mod completions;
mod component;
mod config;
mod docs_static;
mod download;
mod duration;
mod error;
mod fleet;
mod http;
mod install;
mod logging;
mod lu;
mod process;
mod release;
mod report;
mod rollback;
mod self_update;
mod state;
mod status;
mod target;
mod verify;
mod version;

use std::process::ExitCode;

/// Serializes tests that mutate process-wide environment variables.
#[cfg(test)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn main() -> ExitCode {
    match cli::run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("tuishou: {}", crate::http::redact_secrets(&err.to_string()));
            err.exit_code()
        }
    }
}
