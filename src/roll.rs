use crate::component::Component;
use crate::error::{Error, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    #[serde(default)]
    pub node: Vec<Node>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub name: String,
    pub ssh: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub unit: Option<String>,
}

pub fn load_inventory(path_or_list: &str) -> Result<Vec<Node>> {
    let path = Path::new(path_or_list);
    if path.exists() {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let inv: Inventory = toml::from_str(&text).map_err(|e| {
            let msg = e.to_string();
            if msg.contains("unknown field") {
                let key = msg.split('`').nth(1).unwrap_or("unknown").to_string();
                Error::UnknownConfigKey(key)
            } else {
                Error::msg(format!("invalid inventory {}: {e}", path.display()))
            }
        })?;
        return Ok(inv.node);
    }
    // comma-separated list: name@ssh or just ssh? Spec says "file|list" - be simple
    let mut nodes = Vec::new();
    for item in path_or_list
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        if let Some((name, ssh)) = item.split_once('=') {
            nodes.push(Node {
                name: name.to_string(),
                ssh: ssh.to_string(),
                port: None,
                unit: None,
            });
        } else {
            nodes.push(Node {
                name: item.to_string(),
                ssh: item.to_string(),
                port: None,
                unit: None,
            });
        }
    }
    if nodes.is_empty() {
        return Err(Error::msg("no nodes specified"));
    }
    Ok(nodes)
}

#[derive(Debug, Clone)]
pub struct RollPlan {
    pub nodes: String,
    pub component: Component,
    pub version: Option<String>,
    pub batch: usize,
    pub dry_run: bool,
    pub force_after_secs: Option<u64>,
    pub allow_major: bool,
    pub allow_unattested: bool,
    pub quiet: bool,
    #[allow(dead_code)]
    pub verbose: bool,
    pub json: bool,
}

pub trait RollRunner {
    fn drain(&self, node: &Node) -> Result<()>;
    fn wait_drained(&self, node: &Node, force_after_secs: Option<u64>) -> Result<()>;
    fn install_remote(&self, node: &Node, plan: &RollPlan) -> Result<()>;
    fn restart_unit(&self, node: &Node, unit: &str) -> Result<()>;
    fn wait_version(&self, node: &Node, version: &str, timeout_secs: u64) -> Result<()>;
    fn undrain(&self, node: &Node) -> Result<()>;
    fn latest_version(&self, component: &Component) -> Result<String>;
}

pub struct SystemRollRunner;

impl RollRunner for SystemRollRunner {
    fn drain(&self, node: &Node) -> Result<()> {
        run_lu(&["node", "drain", &node.name])
    }

    fn wait_drained(&self, node: &Node, force_after_secs: Option<u64>) -> Result<()> {
        let timeout = Duration::from_secs(force_after_secs.unwrap_or(300));
        let start = Instant::now();
        loop {
            let out = run_lu_capture(&["node", &node.name, "--json"])?;
            if is_drained(&out) {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                if force_after_secs.is_some() {
                    eprintln!(
                        "tuishou: drain wait timed out for {}, continuing due to --force-after",
                        node.name
                    );
                    return Ok(());
                }
                return Err(Error::Roll {
                    node: node.name.clone(),
                    reason: "timed out waiting for node to drain".to_string(),
                });
            }
            thread::sleep(Duration::from_secs(2));
        }
    }

    fn install_remote(&self, node: &Node, plan: &RollPlan) -> Result<()> {
        let mut cmd = vec![
            "tuishou".to_string(),
            "install".to_string(),
            plan.component.name.clone(),
        ];
        if let Some(v) = &plan.version {
            cmd.push("--version".to_string());
            cmd.push(v.clone());
        }
        if plan.allow_unattested {
            cmd.push("--allow-unattested".to_string());
        }
        if plan.allow_major {
            cmd.push("--allow-major".to_string());
        }
        ssh_run(node, &cmd)
    }

    fn restart_unit(&self, node: &Node, unit: &str) -> Result<()> {
        ssh_run(
            node,
            &[
                "systemctl".to_string(),
                "restart".to_string(),
                unit.to_string(),
            ],
        )
    }

    fn wait_version(&self, node: &Node, version: &str, timeout_secs: u64) -> Result<()> {
        let timeout = Duration::from_secs(timeout_secs);
        let start = Instant::now();
        loop {
            let out = run_lu_capture(&["node", &node.name, "--json"]);
            if let Ok(text) = out {
                if text.contains(version) {
                    return Ok(());
                }
            }
            if start.elapsed() >= timeout {
                return Err(Error::Roll {
                    node: node.name.clone(),
                    reason: format!("timed out waiting for version {version}"),
                });
            }
            thread::sleep(Duration::from_secs(2));
        }
    }

    fn undrain(&self, node: &Node) -> Result<()> {
        run_lu(&["node", "undrain", &node.name])
    }

    fn latest_version(&self, component: &Component) -> Result<String> {
        let agent = crate::http::agent();
        let rel = crate::release::fetch_latest(&agent, &component.repo)?;
        Ok(rel.tag)
    }
}

pub fn execute_roll(plan: &RollPlan, runner: &dyn RollRunner) -> Result<()> {
    let nodes = load_inventory(&plan.nodes)?;
    let version = match &plan.version {
        Some(v) => crate::release::normalize_tag(v),
        None => runner.latest_version(&plan.component)?,
    };

    if plan.dry_run {
        print_plan(plan, &nodes, &version)?;
        return Ok(());
    }

    let batch = if plan.batch == 0 { 1 } else { plan.batch };
    for chunk in nodes.chunks(batch) {
        for node in chunk {
            if !plan.quiet {
                eprintln!("tuishou: rolling {} on {}", plan.component.name, node.name);
            }
            roll_one(node, plan, &version, runner).map_err(|e| Error::Roll {
                node: node.name.clone(),
                reason: e.to_string(),
            })?;
            if !plan.quiet {
                eprintln!("tuishou: {} complete", node.name);
            }
        }
    }
    if plan.json {
        println!(
            "{}",
            serde_json::json!({"rolled": nodes.iter().map(|n| &n.name).collect::<Vec<_>>(), "component": plan.component.name, "version": version})
        );
    }
    Ok(())
}

fn roll_one(node: &Node, plan: &RollPlan, version: &str, runner: &dyn RollRunner) -> Result<()> {
    runner.drain(node)?;
    runner.wait_drained(node, plan.force_after_secs)?;
    runner.install_remote(node, plan)?;
    let unit = node
        .unit
        .clone()
        .or_else(|| plan.component.service_unit.clone())
        .unwrap_or_else(|| format!("{}.service", plan.component.name));
    runner.restart_unit(node, &unit)?;
    let wait_secs = plan.force_after_secs.unwrap_or(300);
    runner.wait_version(node, version, wait_secs)?;
    runner.undrain(node)?;
    Ok(())
}

fn print_plan(plan: &RollPlan, nodes: &[Node], version: &str) -> Result<()> {
    if plan.json {
        let items: Vec<_> = nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "node": n.name,
                    "ssh": n.ssh,
                    "port": n.port,
                    "unit": n.unit.clone().or_else(|| plan.component.service_unit.clone()).unwrap_or_else(|| format!("{}.service", plan.component.name)),
                    "component": plan.component.name,
                    "version": version,
                    "steps": ["drain", "wait", "install", "restart", "wait-version", "undrain"],
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&items).map_err(|e| Error::msg(e.to_string()))?
        );
        return Ok(());
    }
    println!(
        "roll plan: {} {} on {} node(s)",
        plan.component.name,
        version,
        nodes.len()
    );
    for n in nodes {
        let unit = n
            .unit
            .clone()
            .or_else(|| plan.component.service_unit.clone())
            .unwrap_or_else(|| format!("{}.service", plan.component.name));
        println!(
            "  - {} ({}) unit={} steps=drain,wait,install,restart,wait-version,undrain",
            n.name, n.ssh, unit
        );
    }
    Ok(())
}

fn run_lu(args: &[&str]) -> Result<()> {
    let lu = find_lu()?;
    let status = Command::new(&lu)
        .args(args)
        .status()
        .map_err(|e| Error::msg(format!("failed to run lu: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "lu {} failed with {}",
            args.join(" "),
            status
        )))
    }
}

fn run_lu_capture(args: &[&str]) -> Result<String> {
    let lu = find_lu()?;
    let out = Command::new(&lu)
        .args(args)
        .output()
        .map_err(|e| Error::msg(format!("failed to run lu: {e}")))?;
    if !out.status.success() {
        return Err(Error::msg(format!(
            "lu {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn find_lu() -> Result<String> {
    let path_var = std::env::var_os("PATH").ok_or_else(|| Error::msg("PATH not set"))?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("lu");
        if cand.is_file() {
            return Ok(cand.to_string_lossy().to_string());
        }
    }
    Err(Error::msg("lu CLI not found on PATH"))
}

fn ssh_run(node: &Node, cmd: &[String]) -> Result<()> {
    let mut args: Vec<String> = Vec::new();
    if let Some(port) = node.port {
        args.push("-p".to_string());
        args.push(port.to_string());
    }
    args.push(node.ssh.clone());
    args.push("--".to_string());
    args.extend(cmd.iter().cloned());
    let status = Command::new("ssh")
        .args(&args)
        .status()
        .map_err(|e| Error::Roll {
            node: node.name.clone(),
            reason: format!("failed to run ssh: {e}"),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Roll {
            node: node.name.clone(),
            reason: format!("ssh command failed with {status}"),
        })
    }
}

fn is_drained(lu_node_json: &str) -> bool {
    // Heuristic: look for running/draining state in JSON. Accept "running_jobs":0 or "jobs":[] or "drained":true.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(lu_node_json) {
        if let Some(d) = v.get("drained").and_then(|x| x.as_bool()) {
            return d;
        }
        if let Some(n) = v.get("running_jobs").and_then(|x| x.as_u64()) {
            return n == 0;
        }
        if let Some(arr) = v.get("jobs").and_then(|x| x.as_array()) {
            return arr.is_empty();
        }
        if let Some(n) = v.get("runningJobs").and_then(|x| x.as_u64()) {
            return n == 0;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract the first fenced ```toml block from a markdown document.
    fn first_toml_block(doc: &str) -> String {
        let mut lines = doc.lines();
        while let Some(line) = lines.next() {
            if line.trim() == "```toml" {
                let mut block = String::new();
                for line in lines.by_ref() {
                    if line.trim() == "```" {
                        return block;
                    }
                    block.push_str(line);
                    block.push('\n');
                }
            }
        }
        panic!("no ```toml fenced block found in docs");
    }

    #[test]
    fn documented_inventory_example_parses() {
        let doc = include_str!("../docs/fleet-roll.md");
        let example = first_toml_block(doc);
        let inventory: Inventory = toml::from_str(&example)
            .unwrap_or_else(|e| panic!("documented inventory example is invalid: {e}\n{example}"));
        assert_eq!(inventory.node.len(), 2);
        assert_eq!(inventory.node[0].name, "node-1");
        assert_eq!(inventory.node[0].ssh, "deploy@10.0.0.11");
        assert_eq!(inventory.node[0].port, Some(22));
        assert_eq!(inventory.node[1].unit.as_deref(), Some("lu-agent.service"));
    }
}
