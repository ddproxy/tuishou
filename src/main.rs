#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match tuishou::cli::run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!(
                "tuishou: {}",
                tuishou::http::redact_secrets(&err.to_string())
            );
            err.exit_code()
        }
    }
}
