use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    X86_64LinuxMusl,
    Aarch64LinuxMusl,
    X86_64Macos,
    Aarch64Macos,
}

impl Target {
    pub fn detect() -> Result<Self> {
        let arch = std::env::consts::ARCH;
        let os = std::env::consts::OS;
        match (os, arch) {
            ("linux", "x86_64") => Ok(Self::X86_64LinuxMusl),
            ("linux", "aarch64") => Ok(Self::Aarch64LinuxMusl),
            ("macos", "x86_64") => Ok(Self::X86_64Macos),
            ("macos", "aarch64") => Ok(Self::Aarch64Macos),
            _ => Err(Error::UnsupportedTarget(format!("{os}-{arch}"))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64LinuxMusl => "x86_64-unknown-linux-musl",
            Self::Aarch64LinuxMusl => "aarch64-unknown-linux-musl",
            Self::X86_64Macos => "x86_64-apple-darwin",
            Self::Aarch64Macos => "aarch64-apple-darwin",
        }
    }

    pub fn is_verified(self) -> bool {
        matches!(self, Self::X86_64LinuxMusl | Self::Aarch64LinuxMusl)
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
