//! Release listing and policy/age/channel eligibility for auto-update.

use crate::auto_update::state::ReleaseCache;
use crate::config::AutoUpdateConfig;
use crate::error::{Error, Result};
use crate::release::{self, Release};
use crate::version::{self, Policy, Version};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct Listing {
    pub releases: Vec<Release>,
    pub etag: Option<String>,
    pub not_modified: bool,
    pub checked_at: String,
}

pub fn fetch_listing(
    agent: &ureq::Agent,
    repo: &str,
    cached: Option<&ReleaseCache>,
    attempts: usize,
) -> Result<Listing> {
    let response = release::fetch_releases(
        agent,
        repo,
        cached.and_then(|c| c.etag.as_deref()),
        attempts,
    )?;
    if response.not_modified {
        let cache = cached.ok_or_else(|| Error::msg("304 without a cached release list"))?;
        return Ok(Listing {
            releases: cache.releases.clone(),
            etag: cache.etag.clone(),
            not_modified: true,
            checked_at: crate::state::now_rfc3339()?,
        });
    }
    let value = response
        .value
        .ok_or_else(|| Error::msg("release listing had no body"))?;
    Ok(Listing {
        releases: release::parse_releases(&value)?,
        etag: response.etag,
        not_modified: false,
        checked_at: crate::state::now_rfc3339()?,
    })
}

pub fn to_cache(listing: &Listing) -> ReleaseCache {
    ReleaseCache {
        etag: listing.etag.clone(),
        checked_at: listing.checked_at.clone(),
        releases: listing.releases.clone(),
    }
}

#[derive(Debug, Clone)]
pub struct Eligibility {
    pub candidate: Option<Release>,
    pub next_eligible_version: Option<String>,
    pub reason: Option<String>,
}

/// Choose the newest release that passes channel, age, policy, bad-version and
/// controller-schema gates. Hold state is handled by the caller so that
/// `status` can still show the next eligible version while held.
pub fn choose(
    listing: &Listing,
    cfg: &AutoUpdateConfig,
    installed: Option<&str>,
    bad_versions: &[String],
    is_controller: bool,
) -> Result<Eligibility> {
    let installed_version = match installed {
        Some(v) => Version::parse(v)?,
        None => {
            return Ok(Eligibility {
                candidate: None,
                next_eligible_version: None,
                reason: Some("component is not installed".to_string()),
            })
        }
    };
    let min_age = crate::duration::parse_duration(&cfg.min_release_age)?;
    let now = OffsetDateTime::now_utc();
    let policy: Policy = cfg.policy.into();
    let mut best: Option<(Version, Release)> = None;
    let mut first_reason: Option<String> = None;

    for rel in &listing.releases {
        if rel.draft {
            continue;
        }
        let version = match Version::parse(&rel.tag) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if cfg.channel == crate::config::Channel::Release && version.is_prerelease() {
            continue;
        }
        if rel.prerelease && cfg.channel == crate::config::Channel::Release {
            continue;
        }
        if bad_versions.iter().any(|b| b == &rel.tag) {
            if first_reason.is_none() {
                first_reason = Some(format!("{} is marked bad", rel.tag));
            }
            continue;
        }
        if !version.is_newer_than(&installed_version) {
            continue;
        }
        if let Some(published) = &rel.published_at {
            match OffsetDateTime::parse(published, &Rfc3339) {
                Ok(published) => {
                    if published + min_age > now {
                        if first_reason.is_none() {
                            first_reason = Some(format!(
                                "{} is younger than min_release_age {}",
                                rel.tag, cfg.min_release_age
                            ));
                        }
                        continue;
                    }
                }
                Err(e) => {
                    if first_reason.is_none() {
                        first_reason =
                            Some(format!("{} has an unparseable published_at ({e})", rel.tag));
                    }
                    continue;
                }
            }
        }
        let (allowed, reason) = version::eligible(
            &version::format_version(&installed_version),
            &rel.tag,
            policy,
            cfg.max_version.as_deref(),
        )?;
        if !allowed {
            let reason = if is_controller
                && policy == Policy::Patch
                && installed_version.major == 0
                && version.major == 0
                && installed_version.minor != version.minor
            {
                format!(
                    "{} is a 0.y minor bump under patch policy (controller schema may change)",
                    rel.tag
                )
            } else {
                reason.unwrap_or_else(|| "policy refuses version".to_string())
            };
            if first_reason.is_none() {
                first_reason = Some(reason);
            }
            continue;
        }
        if is_controller && release::requires_new_database(&rel.body) {
            if first_reason.is_none() {
                first_reason = Some(format!(
                    "{}: {} - controller update requires manual migration",
                    rel.tag,
                    release::SCHEMA_MARKER
                ));
            }
            continue;
        }
        match &best {
            Some((current, _)) if version.cmp_core(current) != std::cmp::Ordering::Greater => {}
            _ => best = Some((version, rel.clone())),
        }
    }

    Ok(match best {
        Some((v, rel)) => Eligibility {
            candidate: Some(rel),
            next_eligible_version: Some(version::format_version(&v)),
            reason: None,
        },
        None => Eligibility {
            candidate: None,
            next_eligible_version: None,
            reason: first_reason.or_else(|| Some("no eligible release".to_string())),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AutoPolicy, Channel, ControllerPolicy};

    fn rel(tag: &str, published: Option<&str>, body: &str, pre: bool) -> Release {
        Release {
            tag: tag.to_string(),
            body: body.to_string(),
            assets: Vec::new(),
            published_at: published.map(str::to_string),
            prerelease: pre,
            draft: false,
        }
    }

    fn cfg() -> AutoUpdateConfig {
        AutoUpdateConfig {
            enabled: true,
            interval: "10m".to_string(),
            components: vec!["lu".to_string()],
            policy: AutoPolicy::Patch,
            min_release_age: "0s".to_string(),
            channel: Channel::Release,
            controller_policy: ControllerPolicy::Manual,
            ..AutoUpdateConfig::default()
        }
    }

    fn listing(releases: Vec<Release>) -> Listing {
        Listing {
            releases,
            etag: None,
            not_modified: false,
            checked_at: "now".to_string(),
        }
    }

    #[test]
    fn patch_policy_picks_highest_patch() {
        let l = listing(vec![
            rel("v0.4.1", None, "", false),
            rel("v0.4.3", None, "", false),
            rel("v0.5.0", None, "", false),
        ]);
        let choice = choose(&l, &cfg(), Some("v0.4.0"), &[], false).unwrap();
        assert_eq!(choice.candidate.unwrap().tag, "v0.4.3");
    }

    #[test]
    fn schema_marker_skips_controller() {
        let l = listing(vec![rel(
            "v0.4.1",
            None,
            "Requires: new controller database\n",
            false,
        )]);
        let result = choose(&l, &cfg(), Some("v0.4.0"), &[], true).unwrap();
        assert!(result.candidate.is_none());
        assert!(result.reason.unwrap().contains("controller database"));
    }

    #[test]
    fn bad_version_ignored_until_newer_appears() {
        let bad = vec!["v0.4.1".to_string()];
        let l = listing(vec![rel("v0.4.1", None, "", false)]);
        assert!(choose(&l, &cfg(), Some("v0.4.0"), &bad, false)
            .unwrap()
            .candidate
            .is_none());
        let l = listing(vec![
            rel("v0.4.1", None, "", false),
            rel("v0.4.2", None, "", false),
        ]);
        assert_eq!(
            choose(&l, &cfg(), Some("v0.4.0"), &bad, false)
                .unwrap()
                .candidate
                .unwrap()
                .tag,
            "v0.4.2"
        );
    }
}
