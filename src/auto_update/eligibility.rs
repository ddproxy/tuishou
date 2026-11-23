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
    let require_published_at = !min_age.is_zero();
    let now = OffsetDateTime::now_utc();
    let policy: Policy = cfg.policy.into();
    let mut candidates: Vec<(Version, Release)> = Vec::new();
    let mut first_reason: Option<String> = None;

    for rel in &listing.releases {
        if rel.draft {
            continue;
        }
        let version = match Version::parse(&rel.tag) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // The only accepted channel is stable. A prerelease is never eligible,
        // regardless of the `prerelease` flag on the release object, and is
        // called out in the first reason so `status` can explain the skip.
        if version.is_prerelease() || rel.prerelease {
            if first_reason.is_none() {
                first_reason = Some(format!("{} is a prerelease; stable channel only", rel.tag));
            }
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
        if require_published_at {
            // An unknown publication time does not establish age. Missing,
            // null and unparseable timestamps are all skipped when an age
            // delay is configured, just as a too-young release is.
            match rel.published_at.as_deref() {
                None => {
                    if first_reason.is_none() {
                        first_reason = Some(format!(
                            "{} has no published_at; cannot satisfy min_release_age {}",
                            rel.tag, cfg.min_release_age
                        ));
                    }
                    continue;
                }
                Some(published) => match OffsetDateTime::parse(published, &Rfc3339) {
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
                },
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
        candidates.push((version, rel.clone()));
    }

    // Sorting before selection makes the result independent of release list
    // order, including among prereleases of the same numeric core.
    candidates.sort_by(|(a, rel_a), (b, rel_b)| a.cmp(b).then_with(|| rel_a.tag.cmp(&rel_b.tag)));
    let best = candidates.pop();

    if let Some((_, winner)) = &best {
        if is_controller {
            if let Some(reason) =
                controller_schema_gate(&listing.releases, &installed_version, winner)
            {
                return Ok(Eligibility {
                    candidate: None,
                    next_eligible_version: None,
                    reason: Some(reason),
                });
            }
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

/// A controller update may not cross any release whose notes are missing or
/// which requires a database migration, including intermediate releases.
/// Missing/unparseable notes fail closed.
fn controller_schema_gate(
    releases: &[Release],
    installed: &Version,
    target: &Release,
) -> Option<String> {
    let target_version = Version::parse(&target.tag).ok()?;
    for rel in releases {
        let Ok(v) = Version::parse(&rel.tag) else {
            continue;
        };
        let greater = v.cmp_core(installed) == std::cmp::Ordering::Greater;
        let at_or_below_target = v.cmp_core(&target_version) != std::cmp::Ordering::Greater;
        if !(greater && at_or_below_target) {
            continue;
        }
        if rel.body.trim().is_empty() {
            return Some(format!(
                "{} has no release notes; refusing to cross an unverified controller schema change",
                rel.tag
            ));
        }
        if release::requires_new_database(&rel.body) {
            return Some(format!(
                "{}: {} - controller update requires manual migration",
                rel.tag,
                release::SCHEMA_MARKER
            ));
        }
    }
    None
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
            target_commitish: None,
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
            channel: Channel::Stable,
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
    fn schema_marker_in_intermediate_release_blocks_controller() {
        let l = listing(vec![
            rel("v0.4.1", None, "notes", false),
            rel("v0.4.2", None, "Requires: new controller database\n", false),
            rel("v0.4.3", None, "safe notes", false),
        ]);
        let mut c = cfg();
        c.policy = crate::config::AutoPolicy::Any;
        let result = choose(&l, &c, Some("v0.4.0"), &[], true).unwrap();
        assert!(result.candidate.is_none());
        assert!(result.reason.unwrap().contains("v0.4.2"));
    }

    #[test]
    fn missing_intermediate_notes_fail_closed_for_controller() {
        let l = listing(vec![
            rel("v0.4.1", None, "", false),
            rel("v0.4.2", None, "safe", false),
        ]);
        let result = choose(&l, &cfg(), Some("v0.4.0"), &[], true).unwrap();
        assert!(result.candidate.is_none());
        assert!(result.reason.unwrap().contains("no release notes"));
    }

    #[test]
    fn missing_published_at_is_skipped_when_age_delay_is_configured() {
        let mut aged = cfg();
        aged.min_release_age = "10m".to_string();
        let l = listing(vec![rel("v0.4.1", None, "", false)]);
        let skipped = choose(&l, &aged, Some("v0.4.0"), &[], false).unwrap();
        assert!(skipped.candidate.is_none());
        assert!(
            skipped.reason.unwrap().contains("published_at"),
            "unknown publication time must not establish age"
        );

        let l = listing(vec![rel("v0.4.1", Some("2020-01-01T00:00:00Z"), "", false)]);
        assert_eq!(
            choose(&l, &aged, Some("v0.4.0"), &[], false)
                .unwrap()
                .candidate
                .unwrap()
                .tag,
            "v0.4.1"
        );

        // Without an age delay, a missing publication time does not matter.
        assert_eq!(
            choose(
                &listing(vec![rel("v0.4.1", None, "", false)]),
                &cfg(),
                Some("v0.4.0"),
                &[],
                false
            )
            .unwrap()
            .candidate
            .unwrap()
            .tag,
            "v0.4.1"
        );
    }

    #[test]
    fn prereleases_are_refused_on_the_stable_channel() {
        let c = cfg();
        // Both the release flag and a prerelease tag are refused.
        let l = listing(vec![
            rel("v1.2.1-rc1", None, "", false),
            rel("v1.2.2", None, "", true),
        ]);
        let choice = choose(&l, &c, Some("v1.2.0"), &[], false).unwrap();
        assert!(choice.candidate.is_none());
        let reason = choice.reason.unwrap();
        assert!(reason.contains("prerelease"), "{reason}");
        assert!(reason.contains("stable channel only"), "{reason}");
    }

    #[test]
    fn candidate_selection_uses_semver_precedence_not_list_order() {
        let c = cfg();
        let l = listing(vec![
            rel("v1.2.1", None, "notes", false),
            rel("v1.2.3", None, "notes", false),
            rel("v1.2.2", None, "notes", false),
        ]);
        for reversed in [false, true] {
            let mut releases = l.releases.clone();
            if reversed {
                releases.reverse();
            }
            let choice = choose(&listing(releases), &c, Some("v1.2.0"), &[], false).unwrap();
            assert_eq!(choice.candidate.unwrap().tag, "v1.2.3");
        }

        // A prerelease never wins even when it sorts above the stable release.
        let l = listing(vec![
            rel("v1.2.1-rc2", None, "", true),
            rel("v1.2.1", None, "notes", false),
        ]);
        let choice = choose(&l, &c, Some("v1.2.0"), &[], false).unwrap();
        assert_eq!(choice.candidate.unwrap().tag, "v1.2.1");
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
