#![no_main]

use libfuzzer_sys::fuzz_target;
use tuishou::release::{ensure_requested_tag, parse_release, parse_releases};

fuzz_target!(|data: &[u8]| {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let Ok(release) = parse_release(value.clone()) else {
        return;
    };

    // A decoded release must survive serialization and decoding again: asset
    // list, flags and optional fields are unchanged.
    let encoded = serde_json::to_value(&release).expect("Release serializes");
    let decoded = parse_release(encoded).expect("serialized Release parses");
    assert_eq!(release, decoded);

    let array = serde_json::Value::Array(vec![value]);
    if let Ok(list) = parse_releases(&array) {
        assert_eq!(list, vec![release.clone()]);
    }

    // The requested-tag binding accepts the canonical form of the tag that was
    // decoded from the wire response.
    if !release.tag.is_empty() {
        let requested = if release.tag.starts_with('v') {
            release.tag.clone()
        } else {
            format!("v{}", release.tag)
        };
        assert!(ensure_requested_tag(&release, &requested).is_ok());
    }
});
