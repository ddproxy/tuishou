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
    let good = state::load_known_good(&state_path)?;

    let mut restored_any = false;
    for name in &component.binaries {
        // A binary without a backup was never installed by tuishou. Restoring
        // only backed-up names keeps rollback safe for partial installs.
        let prev = bin_dir.join(format!("{name}.prev"));
        if !prev.is_file() {
            continue;
        }
        let Some(expected) = good.checksums.get(name) else {
            return Err(Error::msg(format!(
                "refusing rollback: {}.prev has no recorded good checksum",
                name
            )));
        };
        let actual = crate::verify::sha256_file(&prev)?;
        if actual != *expected {
            return Err(Error::msg(format!(
                "refusing rollback: {}.prev does not match the recorded good checksum",
                name
            )));
        }
        install::restore_previous_binary(&bin_dir, name)?;
        restored_any = true;
    }
    if !restored_any {
        return Err(Error::NoPrevious(component.name.clone()));
    }

    if update_state {
        if let Some(entry) = st.components.get_mut(&component.name) {
            let current = entry.version.clone();
            let restored = entry
                .previous_version
                .take()
                .unwrap_or_else(|| "restored".into());
            entry.previous_version = Some(current);
            // Never leave the bad tag recorded as the installed version when
            // no previous version is known.
            entry.version = restored;
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
        config_path: None,
        auto_update: false,
    };
    let _ = quiet;
    restore_binaries(&c, prefix, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::InstalledComponent;
    use std::fs;

    fn component() -> Component {
        Component {
            name: "lu".to_string(),
            repo: String::new(),
            asset_pattern: String::new(),
            binaries: vec!["lu".to_string()],
            service_unit: None,
            binary_ranges: Vec::new(),
            config_path: None,
            auto_update: false,
        }
    }

    fn setup(prefix: &Path, prev: &[u8], record_good: bool) {
        let mut st = state::State::default();
        st.components.insert(
            "lu".to_string(),
            InstalledComponent {
                version: "v0.5.0".to_string(),
                target: "test".to_string(),
                checksum: "a".repeat(64),
                installed_at: "2026-01-01T00:00:00Z".to_string(),
                previous_version: Some("v0.4.0".to_string()),
            },
        );
        state::save(&state::default_state_path(Some(prefix)), &st).unwrap();
        fs::write(prefix.join("lu"), b"installed").unwrap();
        fs::write(prefix.join("lu.prev"), prev).unwrap();
        if record_good {
            let mut good = state::KnownGood::default();
            good.checksums.insert(
                "lu".to_string(),
                crate::verify::sha256_file(&prefix.join("lu.prev")).unwrap(),
            );
            state::save_known_good(&state::default_state_path(Some(prefix)), &good).unwrap();
        }
    }

    #[test]
    fn rollback_refuses_tampered_good_backup() {
        let td = tempfile::tempdir().unwrap();
        setup(td.path(), b"tampered", true);
        let mut good = state::KnownGood::default();
        good.checksums.insert("lu".to_string(), "b".repeat(64));
        state::save_known_good(&state::default_state_path(Some(td.path())), &good).unwrap();

        let err = restore_binaries(&component(), Some(td.path()), true).unwrap_err();
        assert!(err.to_string().contains("recorded good checksum"), "{err}");
        assert_eq!(fs::read(td.path().join("lu")).unwrap(), b"installed");
    }

    #[test]
    fn rollback_refuses_backup_without_good_record() {
        let td = tempfile::tempdir().unwrap();
        setup(td.path(), b"good", false);
        let err = restore_binaries(&component(), Some(td.path()), true).unwrap_err();
        assert!(
            err.to_string().contains("no recorded good checksum"),
            "{err}"
        );
        assert_eq!(fs::read(td.path().join("lu")).unwrap(), b"installed");
    }

    #[test]
    fn rollback_records_consistent_version_without_previous() {
        let td = tempfile::tempdir().unwrap();
        setup(td.path(), b"good", true);
        let state_path = state::default_state_path(Some(td.path()));
        let mut st = state::load(&state_path).unwrap();
        if let Some(entry) = st.components.get_mut("lu") {
            entry.previous_version = None;
        }
        state::save(&state_path, &st).unwrap();

        restore_binaries(&component(), Some(td.path()), true).unwrap();
        let st = state::load(&state_path).unwrap();
        assert_eq!(st.components["lu"].version, "restored");
        assert_eq!(
            st.components["lu"].previous_version.as_deref(),
            Some("v0.5.0")
        );
        assert_eq!(fs::read(td.path().join("lu")).unwrap(), b"good");
    }
}
