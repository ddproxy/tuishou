#![no_main]

use libfuzzer_sys::fuzz_target;
use tuishou::component::builtin_components;
use tuishou::config::ConfigFile;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(cfg) = toml::from_str::<ConfigFile>(text) else {
        return;
    };

    // Validation must stay total for every document serde accepts. Repeating it
    // must give the same answer, including when a health URL checks for a local listener.
    let first = (
        cfg.auto_update.validate().is_ok(),
        cfg.update.validate().is_ok(),
        cfg.verify.validate(&builtin_components()).is_ok(),
        cfg.top.validate().is_ok(),
    );
    let again = toml::from_str::<ConfigFile>(text).expect("same TOML parses twice");
    let second = (
        again.auto_update.validate().is_ok(),
        again.update.validate().is_ok(),
        again.verify.validate(&builtin_components()).is_ok(),
        again.top.validate().is_ok(),
    );
    assert_eq!(first, second);

    // Parsing is stable across runs, and a document that validates obeys the
    // safety rules the validator promises (unknown keys cannot get this far).
    assert_eq!(cfg.auto_update.enabled, again.auto_update.enabled);
    assert_eq!(cfg.auto_update.channel, again.auto_update.channel);
    assert_eq!(cfg.auto_update.policy, again.auto_update.policy);
    assert_eq!(
        cfg.auto_update.controller_policy,
        again.auto_update.controller_policy
    );
    assert_eq!(cfg.auto_update.interval, again.auto_update.interval);
    if first.0 {
        assert!(cfg.auto_update.require_attestation);
    }
});
