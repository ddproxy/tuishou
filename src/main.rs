#![forbid(unsafe_code)]

mod archive;
mod cli;
mod completions;
mod component;
mod config;
mod docs_static;
mod download;
mod error;
mod http;
mod install;
mod release;
mod roll;
mod rollback;
mod self_update;
mod state;
mod status;
mod target;
mod verify;

use std::process::ExitCode;

fn main() -> ExitCode {
    match cli::run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("tuishou: {err}");
            err.exit_code()
        }
    }
}
