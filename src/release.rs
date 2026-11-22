use crate::error::{Error, Result};
use crate::http::{self, ConditionalJson};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub body: String,
    pub assets: Vec<ReleaseAsset>,
    #[serde(default)]
    pub published_at: Option<String>,
    /// Commitish the tag points at. Used to verify the source digest extension
    /// in Fulcio certificates.
    #[serde(default)]
    pub target_commitish: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
}

pub fn github_api_base() -> String {
    std::env::var("TUISHOU_GITHUB_API_BASE")
        .unwrap_or_else(|_| "https://api.github.com".to_string())
}

pub fn normalize_tag(version: &str) -> String {
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    }
}

/// Marker that the Lu release notes carry when the controller database schema
/// changes. Auto-update refuses to cross such a release; humans must handle
/// the migration.
pub const SCHEMA_MARKER: &str = "Requires: new controller database";

pub fn requires_new_database(body: &str) -> bool {
    body.lines().any(|line| line.trim() == SCHEMA_MARKER)
}

pub fn fetch_latest(agent: &ureq::Agent, repo: &str) -> Result<Release> {
    let url = format!("{}/repos/{repo}/releases/latest", github_api_base());
    let value = http::get_json(agent, &url).map_err(|e| match e {
        Error::Http(m) if m.contains("status 404") => Error::ReleaseNotFound {
            component: repo.to_string(),
            version: "latest".to_string(),
        },
        other => other,
    })?;
    parse_release(value)
}

pub fn fetch_tagged(agent: &ureq::Agent, repo: &str, version: &str) -> Result<Release> {
    let tag = normalize_tag(version);
    let url = format!("{}/repos/{repo}/releases/tags/{tag}", github_api_base());
    let value = http::get_json(agent, &url).map_err(|e| match e {
        Error::Http(m) if m.contains("status 404") => Error::ReleaseNotFound {
            component: repo.to_string(),
            version: tag.clone(),
        },
        other => other,
    })?;
    parse_release(value)
}

/// List releases with a conditional request. `attempts` controls how many
/// times a rate-limited response is retried.
pub fn fetch_releases(
    agent: &ureq::Agent,
    repo: &str,
    etag: Option<&str>,
    attempts: usize,
) -> Result<ConditionalJson> {
    let url = format!("{}/repos/{repo}/releases?per_page=30", github_api_base());
    http::get_json_conditional(agent, &url, etag, attempts)
}

pub fn parse_release(value: serde_json::Value) -> Result<Release> {
    let tag = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Http("release missing tag_name".to_string()))?
        .to_string();
    let body = value
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let assets = value
        .get("assets")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Http("release missing assets".to_string()))?
        .iter()
        .filter_map(|a| {
            let name = a.get("name")?.as_str()?.to_string();
            let url = a
                .get("browser_download_url")
                .or_else(|| a.get("url"))
                .and_then(|v| v.as_str())?
                .to_string();
            Some(ReleaseAsset { name, url })
        })
        .collect();
    Ok(Release {
        tag,
        body,
        assets,
        published_at: value
            .get("published_at")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        target_commitish: value
            .get("target_commitish")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        prerelease: value
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        draft: value
            .get("draft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

pub fn parse_releases(value: &serde_json::Value) -> Result<Vec<Release>> {
    let arr = value
        .as_array()
        .ok_or_else(|| Error::Http("releases response is not an array".to_string()))?;
    arr.iter().cloned().map(parse_release).collect()
}

pub fn find_asset<'a>(release: &'a Release, name: &str) -> Result<&'a ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| Error::Http(format!("release {} missing asset {name}", release.tag)))
}

/// Resolve the release's source commit to a SHA so attestation certificates can
/// be checked against the source digest extension. Handles both the case where
/// `target_commitish` is already a SHA and the common case where it is a branch
/// or ref name.
pub fn fetch_source_digest(
    agent: &ureq::Agent,
    repo: &str,
    release: &Release,
) -> Result<Option<String>> {
    let Some(commitish) = release.target_commitish.as_deref() else {
        return Ok(None);
    };
    if is_sha1(commitish) {
        return Ok(Some(commitish.to_ascii_lowercase()));
    }
    let ref_path = if let Some(rest) = commitish.strip_prefix("refs/") {
        rest.to_string()
    } else {
        format!("heads/{commitish}")
    };
    let url = format!("{}/repos/{repo}/git/ref/{ref_path}", github_api_base());
    let value = http::get_json(agent, &url)?;
    let object = value
        .get("object")
        .ok_or_else(|| Error::Http(format!("git ref {ref_path} response has no object")))?;
    let mut sha = object
        .get("sha")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Http(format!("git ref {ref_path} response has no object sha")))?
        .to_string();
    // Annotated tags point at a tag object; follow it to the commit.
    for _ in 0..3 {
        if object.get("type").and_then(|v| v.as_str()) != Some("tag") {
            break;
        }
        let tag_url = format!("{}/repos/{repo}/git/tags/{sha}", github_api_base());
        let tag = http::get_json(agent, &tag_url)?;
        let next = tag
            .get("object")
            .and_then(|o| o.get("sha"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Http("tag object has no target sha".to_string()))?
            .to_string();
        if next == sha {
            break;
        }
        sha = next;
    }
    if !is_sha1(&sha) {
        return Err(Error::Http(format!(
            "could not resolve {repo} {ref_path} to a commit sha"
        )));
    }
    Ok(Some(sha.to_ascii_lowercase()))
}

fn is_sha1(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn changelog_heading(body: &str, tag: &str) -> Option<String> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            if heading.contains(version) {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_marker_requires_whole_line() {
        assert!(requires_new_database(
            "notes\nRequires: new controller database\n"
        ));
        assert!(!requires_new_database(
            "Requires: new controller database soon"
        ));
    }
}
