use crate::component::Component;
use crate::error::{Error, Result};
use crate::install;
use crate::state;
use std::path::Path;

/// Restore `<name>.prev` over every binary of a component.
///
/// `update_state` is true for a deliberate `tuishou rollback` command and
/// false for an automatic recovery, where the recorded version must stay at
/// the last known good value. The good `.prev` bytes are always preserved.
pub fn restore_binaries(
    component: &Component,
    prefix: Option<&Path>,
    update_state: bool,
) -> Result<()> {
    let bin_dir = install::install_dir(prefix);
    let state_path = state::default_state_path(prefix);
    let mut st = state::load(&state_path)?;
    if !st.components.contains_key(&component.name) {
        return Err(Error::UnknownComponent(component.name.clone()));
    }

    let mut restored_any = false;
    for name in &component.binaries {
        install::restore_previous_binary(&bin_dir, name)?;
        restored_any = true;
    }
    if !restored_any {
        return Err(Error::NoPrevious(component.name.clone()));
    }

    if update_state {
        if let Some(entry) = st.components.get_mut(&component.name) {
            if let Some(prev_version) = entry.previous_version.clone() {
                entry.previous_version = Some(entry.version.clone());
                entry.version = prev_version;
            }
        }
        state::save(&state_path, &st)?;
    }
    Ok(())
}

/// Backward-compatible wrapper for the manual rollback CLI.
pub fn rollback_binaries(
    component: &str,
    binaries: &[String],
    prefix: Option<&Path>,
    quiet: bool,
) -> Result<()> {
    let c = Component {
        name: component.to_string(),
        repo: String::new(),
        asset_pattern: String::new(),
        binaries: binaries.to_vec(),
        service_unit: None,
        binary_ranges: Vec::new(),
    };
    let _ = quiet;
    restore_binaries(&c, prefix, true)
}
