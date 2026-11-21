use crate::error::{Error, Result};
use crate::install;
use crate::state;
use std::fs;
use std::path::Path;

pub fn rollback_binaries(
    component: &str,
    binaries: &[String],
    prefix: Option<&Path>,
    quiet: bool,
) -> Result<()> {
    let bin_dir = install::install_dir(prefix);
    let state_path = state::default_state_path(prefix);
    let mut st = state::load(&state_path)?;
    if !st.components.contains_key(component) {
        return Err(Error::UnknownComponent(component.to_string()));
    }

    let mut restored_any = false;
    for name in binaries {
        let dest = bin_dir.join(name);
        let prev = bin_dir.join(format!("{name}.prev"));
        if !prev.exists() {
            continue;
        }
        let tmp = bin_dir.join(format!(".{name}.rollback-{}", std::process::id()));
        if dest.exists() {
            fs::copy(&dest, &tmp).ok();
        }
        fs::rename(&prev, &dest).map_err(|source| Error::Write {
            path: dest.clone(),
            source,
        })?;
        // old dest becomes prev? Spec just says "restore <name>.prev" - keep it simple
        if tmp.exists() {
            let _ = fs::rename(&tmp, &prev);
        }
        restored_any = true;
        if !quiet {
            println!("rolled back {name}");
        }
    }
    if !restored_any {
        return Err(Error::NoPrevious(component.to_string()));
    }

    if let Some(entry) = st.components.get_mut(component) {
        if let Some(prev_version) = entry.previous_version.clone() {
            entry.previous_version = Some(entry.version.clone());
            entry.version = prev_version;
        }
    }
    state::save(&state_path, &st)?;
    Ok(())
}
