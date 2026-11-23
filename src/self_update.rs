use crate::component::Component;
use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

pub const SELF_COMPONENT: &str = "tuishou";

/// Component describing our own release archive. The repository and asset are
/// built in; a config entry with the same name may only change paths/policy.
pub fn self_component(components: &[Component]) -> Component {
    components
        .iter()
        .find(|c| c.name == SELF_COMPONENT)
        .cloned()
        .unwrap_or_else(|| Component {
            name: SELF_COMPONENT.to_string(),
            repo: "ddproxy/tuishou".to_string(),
            asset_pattern: "tuishou-{tag}-{target}.tar.gz".to_string(),
            binaries: vec![SELF_COMPONENT.to_string()],
            service_unit: None,
            binary_ranges: Vec::new(),
            config_path: None,
            auto_update: false,
        })
}

/// Where `self-update` writes: the directory containing the running binary,
/// unless the caller supplies an explicit prefix (useful for staging/tests).
pub fn binary_dir(prefix: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = prefix {
        return Ok(p.to_path_buf());
    }
    let exe = std::env::current_exe()
        .map_err(|source| Error::msg(format!("cannot locate running tuishou binary: {source}")))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::msg("running tuishou binary has no parent directory"))
}
