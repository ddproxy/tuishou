#![no_main]

use libfuzzer_sys::fuzz_target;
use std::time::Duration;
use tuishou::lu::{
    controller_idle, count_unavailable_nodes, node_is_drained, normalize_version, parse_nodes,
    parse_self, self_is_healthy, status_reports_version,
};

const TAG: &str = "v0.0.0";

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // `lu nodes --json` is semi-trusted controller output. A decoded list must
    // agree with the unavailable count, and a drained node is always one that
    // explicitly reported draining.
    if let Ok(nodes) = parse_nodes(text) {
        let unavailable = count_unavailable_nodes(text).expect("same JSON parses twice");
        assert!(unavailable <= nodes.len());
        for node in &nodes {
            if node_is_drained(node) {
                assert_eq!(node.draining, Some(true));
                assert!(node.current_job.is_none());
            }
        }
    }

    // Health and idleness checks must never panic on malformed child output.
    // When health succeeds, the version and online assertions below must hold.
    controller_idle(text);
    status_reports_version(text, TAG);
    if self_is_healthy(text, TAG, Duration::from_secs(60)) {
        let status = parse_self(text).expect("healthy status parses");
        let version = status
            .version
            .as_deref()
            .expect("healthy status has version");
        assert_eq!(normalize_version(version), normalize_version(TAG));
        assert_ne!(status.online, Some(false));
    }
});
