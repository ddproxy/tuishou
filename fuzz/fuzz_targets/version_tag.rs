#![no_main]

use libfuzzer_sys::fuzz_target;
use std::cmp::Ordering;
use tuishou::version::{eligible, format_version, Policy, Version};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(version) = Version::parse(text) else {
        return;
    };

    // Formatting a decoded tag and decoding it again preserves core and
    // pre-release identifiers. A version never ranks above itself, and no
    // policy may promote it onto itself.
    let rendered = format_version(&version);
    let reparsed = Version::parse(&rendered).expect("formatted version parses");
    assert_eq!(version, reparsed);
    assert_eq!(version.cmp(&version), Ordering::Equal);
    for policy in [Policy::Patch, Policy::Minor, Policy::Any] {
        let (allowed, _) = eligible(&rendered, &rendered, policy, None)
            .expect("same-version eligibility is decidable");
        assert!(!allowed);
        let (capped, _) = eligible(&rendered, &rendered, policy, Some(&rendered))
            .expect("capped eligibility is decidable");
        assert!(!capped);
    }
});
