//! Parsers for the real Lu 0.4 JSON shapes.
//!
//! `lu nodes --json` is the only source for node version and drain state.
//! Reported versions have no leading `v`; release tags do. Matching is exact
//! after stripping a single leading `v`.

use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Clone, Deserialize)]
pub struct NodeStatus {
    #[allow(dead_code)]
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub draining: Option<bool>,
    #[serde(default)]
    pub current_job: Option<serde_json::Value>,
    #[serde(default)]
    pub online: Option<bool>,
    #[serde(default)]
    /// Lu reports this as a unix timestamp in seconds; older shapes used an RFC 3339 string.
    pub last_seen: Option<serde_json::Value>,
}

pub fn parse_nodes(json: &str) -> Result<Vec<NodeStatus>, String> {
    serde_json::from_str::<Vec<NodeStatus>>(json).map_err(|e| e.to_string())
}

#[allow(dead_code)] // operator-side parser, kept for operator views of `lu nodes --json`
pub fn find_node(json: &str, node: &str) -> Result<NodeStatus, String> {
    let nodes = parse_nodes(json)?;
    nodes
        .into_iter()
        .find(|n| n.name == node)
        .ok_or_else(|| format!("node {node} was not reported by lu nodes --json"))
}

/// A node is drained only when Lu explicitly reports `draining: true` and no
/// current job. An idle node that is not drained has not been confirmed safe
/// by the drain command, so unknown or partial shapes fail closed.
pub fn node_is_drained(node: &NodeStatus) -> bool {
    node.draining == Some(true) && node.current_job.is_none()
}

#[allow(dead_code)] // operator-side parser, kept for operator views of `lu nodes --json`
pub fn nodes_report_drained(json: &str, node: &str) -> bool {
    find_node(json, node)
        .map(|n| node_is_drained(&n))
        .unwrap_or(false)
}

/// A node counts against the fleet limit while it is draining or reported
/// offline. Unknown state fails closed: only an explicit `online: false` or
/// `draining: true` is counted, so other node conditions do not block updates.
pub fn node_is_unavailable(node: &NodeStatus) -> bool {
    node.draining == Some(true) || node.online == Some(false)
}

/// Count unavailable nodes in the `lu nodes --json` payload.
pub fn count_unavailable_nodes(json: &str) -> Result<usize, String> {
    Ok(parse_nodes(json)?
        .iter()
        .filter(|n| node_is_unavailable(n))
        .count())
}

pub fn normalize_version(value: &str) -> &str {
    value.trim().strip_prefix('v').unwrap_or(value.trim())
}

/// Compare the exact `version` field of the named node with a release tag.
#[allow(dead_code)] // operator-side parser, kept for operator views of `lu nodes --json`
pub fn node_reports_version(json: &str, node: &str, tag: &str) -> bool {
    find_node(json, node)
        .ok()
        .and_then(|n| n.version)
        .map(|reported| normalize_version(&reported) == normalize_version(tag))
        .unwrap_or(false)
}

/// A node must report both the exact version and a recent heartbeat. Missing
/// or unparseable timestamps fail closed so an offline node cannot pass health.
#[allow(dead_code)] // operator-side parser, kept for operator views of `lu nodes --json`
pub fn node_healthy(json: &str, node: &str, tag: &str, max_age: std::time::Duration) -> bool {
    if !node_reports_version(json, node, tag) {
        return false;
    }
    let Ok(status) = find_node(json, node) else {
        return false;
    };
    status_is_fresh(&status, max_age)
}

/// The heartbeat time of a node, from either a unix timestamp or an RFC 3339 string.
fn last_seen_time(status: &NodeStatus) -> Option<OffsetDateTime> {
    match status.last_seen.as_ref()? {
        serde_json::Value::Number(n) => {
            let secs = n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?;
            OffsetDateTime::from_unix_timestamp(secs).ok()
        }
        serde_json::Value::String(s) => OffsetDateTime::parse(s, &Rfc3339).ok(),
        _ => None,
    }
}

/// A status counts as fresh only with a readable heartbeat that is not in the future and not
/// older than `max_age`; anything else fails closed.
pub fn status_is_fresh(status: &NodeStatus, max_age: std::time::Duration) -> bool {
    let Some(seen) = last_seen_time(status) else {
        return false;
    };
    let now = OffsetDateTime::now_utc();
    now >= seen && (now - seen) <= max_age
}

/// Parse the node's own view as printed by `ting status --json` (one object with the same fields
/// as an entry of `lu nodes --json`).
pub fn parse_self(json: &str) -> Result<NodeStatus, String> {
    serde_json::from_str::<NodeStatus>(json).map_err(|e| e.to_string())
}

/// The node's own view reports drained only when draining is true and no job is current.
pub fn self_is_drained(json: &str) -> bool {
    parse_self(json)
        .map(|n| node_is_drained(&n))
        .unwrap_or(false)
}

/// The node's own view reports the exact version, is online and has a fresh heartbeat.
pub fn self_is_healthy(json: &str, tag: &str, max_age: std::time::Duration) -> bool {
    let Ok(status) = parse_self(json) else {
        return false;
    };
    let version_ok = status
        .version
        .as_deref()
        .map(|v| normalize_version(v) == normalize_version(tag))
        .unwrap_or(false);
    version_ok && status.online != Some(false) && status_is_fresh(&status, max_age)
}

/// `lu status --json` reports a controller version and a `jobs` object.
pub fn status_reports_version(json: &str, tag: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("version")
                .and_then(|s| s.as_str())
                .map(|reported| normalize_version(reported) == normalize_version(tag))
        })
        .unwrap_or(false)
}

/// A minimal reachability signal: the controller answered with a status
/// document that includes its version. Idleness is checked separately.
#[allow(dead_code)] // operator-side parser, kept for operator views of `lu nodes --json`
pub fn status_available(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("version")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .is_some()
}

/// Controller idleness requires all four busy counters to be present,
/// non-negative integers and zero. Every other shape fails closed.
pub fn controller_idle(json: &str) -> bool {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let Some(jobs) = value.get("jobs").and_then(|v| v.as_object()) else {
        return false;
    };
    ["leased", "running", "recovering", "needs_input"]
        .iter()
        .all(|key| {
            jobs.get(*key)
                .and_then(|v| v.as_u64())
                .map(|n| n == 0)
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODES: &str = r#"[
      {"name":"n1","version":"0.4.0","draining":true,"online":true,"current_job":null,
       "last_seen":"2026-01-01T00:00:00Z","harness":"systemd","labels":[],"cordons":[],
       "cordon_details":[],"stuck":false,"load1":0.1},
      {"name":"n2","version":"0.4.1","draining":false,"online":true,
       "current_job":{"id":"j1"},"last_seen":"2026-01-01T00:00:00Z","harness":"systemd",
       "labels":[],"cordons":[],"cordon_details":[],"stuck":false,"load1":0.2},
      {"name":"n3","version":"0.4.10","draining":false,"online":true,"current_job":null},
      {"name":"n4","version":"0.4.1-rc1","current_job":null},
      {"name":"n5"}
    ]"#;

    #[test]
    fn drain_state_uses_real_nodes_shape() {
        assert!(nodes_report_drained(NODES, "n1"));
        assert!(!nodes_report_drained(NODES, "n2"));
        // `draining` must be explicitly reported by Lu; an idle node is not
        // proof that the drain command succeeded.
        assert!(!nodes_report_drained(NODES, "n5"));
        assert!(!nodes_report_drained(NODES, "missing"));
        assert!(!nodes_report_drained("{}", "n1"));
    }

    #[test]
    fn unavailable_count_includes_draining_and_offline_nodes() {
        assert_eq!(count_unavailable_nodes(NODES).unwrap(), 1);
        let json = r#"[
          {"name":"a","draining":true,"online":true},
          {"name":"b","draining":false,"online":false},
          {"name":"c","draining":false,"online":true},
          {"name":"d"}
        ]"#;
        assert_eq!(count_unavailable_nodes(json).unwrap(), 2);
        assert!(count_unavailable_nodes("not json").is_err());
    }

    #[test]
    fn version_match_is_named_exact_and_normalized() {
        assert!(node_reports_version(NODES, "n1", "v0.4.0"));
        assert!(node_reports_version(NODES, "n1", "0.4.0"));
        assert!(!node_reports_version(NODES, "n3", "v0.4.1"));
        assert!(!node_reports_version(NODES, "n4", "v0.4.1"));
        assert!(!node_reports_version(NODES, "n2", "v0.4.0"));
        assert!(!node_reports_version(NODES, "missing", "v0.4.0"));
    }

    #[test]
    fn controller_idleness_fails_closed() {
        let idle = r#"{"jobs":{"blocked":9,"cancelled":31,"done":187,"failed":54,"leased":0,"needs_input":0,"queued":0,"recovering":0,"running":0},"nodes":5,"nodes_online":5,"schema_version":8,"version":"0.4.0"}"#;
        assert!(controller_idle(idle));
        assert!(status_reports_version(idle, "v0.4.0"));
        assert!(!status_reports_version(idle, "0.4.1"));
        assert!(!controller_idle(
            r#"{"jobs":{"leased":0,"running":4,"recovering":0,"needs_input":0}}"#
        ));
        assert!(!controller_idle(r#"{"jobs":{"leased":0,"running":0}}"#));
        assert!(!controller_idle(
            r#"{"jobs":{"leased":0,"running":0,"recovering":0,"needs_input":-1}}"#
        ));
        assert!(!controller_idle(
            r#"{"jobs":{"leased":0,"running":1,"recovering":0,"needs_input":0}}"#
        ));
        assert!(!controller_idle(r#"{"running_jobs":0,"jobs":[]}"#));
        assert!(!controller_idle("not json"));
    }
}
