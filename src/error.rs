use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

pub const EXIT_ERROR: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_UPDATES_AVAILABLE: u8 = 10;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),

    /// Bad command-line input. Distinct from a verification refusal so the
    /// process can exit 2 for usage errors and 1 for failed verification.
    #[error("{0}")]
    Usage(String),

    #[error("unknown component '{0}'")]
    UnknownComponent(String),

    #[error("unknown config key '{0}'")]
    UnknownConfigKey(String),

    #[error("config error in {path}: {source}")]
    Config {
        path: PathBuf,
        #[source]
        source: Box<Error>,
    },

    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("http error: {0}")]
    Http(String),

    #[error("release not found for {component} {version}")]
    ReleaseNotFound { component: String, version: String },

    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    #[error("attestation verification failed: {0}")]
    Attestation(String),

    #[error("attestation refused: {0}")]
    AttestationRefused(String),

    #[error("attestation verification unavailable: {0}")]
    AttestationUnavailable(String),

    #[error("archive error: {0}")]
    Archive(String),

    #[error("download exceeded size limit of {limit} bytes")]
    DownloadTooLarge { limit: u64 },

    #[error("unsupported target '{0}'")]
    UnsupportedTarget(String),

    #[error("major version change from {installed} to {latest} requires --allow-major")]
    MajorVersion { installed: String, latest: String },

    #[error("no previous version recorded for '{0}'")]
    NoPrevious(String),

    #[error("roll failed on node '{node}': {reason}")]
    Roll { node: String, reason: String },
}

impl Error {
    pub fn msg(msg: impl Into<String>) -> Self {
        Self::Message(msg.into())
    }

    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Message(m) if m == "updates available" => ExitCode::from(EXIT_UPDATES_AVAILABLE),
            Self::Usage(_) => ExitCode::from(EXIT_USAGE),
            _ => ExitCode::from(EXIT_ERROR),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
