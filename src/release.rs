use crate::error::{Error, Result};
use crate::http;

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub body: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone)]
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

fn parse_release(value: serde_json::Value) -> Result<Release> {
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
    Ok(Release { tag, body, assets })
}

pub fn find_asset<'a>(release: &'a Release, name: &str) -> Result<&'a ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| Error::Http(format!("release {} missing asset {name}", release.tag)))
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
