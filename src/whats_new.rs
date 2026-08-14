//! Exact-version release notes with an immediate embedded changelog fallback.
//!
//! A release announcement must never delay startup or describe a version other
//! than the binary the user actually launched. The checked-in changelog is
//! therefore embedded into every binary; GitHub can enrich it asynchronously.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use semver::Version;
use serde::Deserialize;

const EMBEDDED_CHANGELOG: &str = include_str!("../CHANGELOG.md");
const REPOSITORY: &str = "codersauce/red";
const MAX_RELEASE_NOTES_BYTES: usize = 128 * 1024;
const RELEASE_REQUEST_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_HIGHLIGHTS_PER_SECTION: usize = 5;

/// One version-specific release announcement ready for terminal presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseNotes {
    /// Exact package version embedded in the running binary.
    pub version: String,
    /// One or more relevant changelog sections, newest first.
    pub markdown: String,
    /// Canonical browser destination for the matching release.
    pub release_url: String,
    /// Human-readable publication date when GitHub supplies one.
    pub published_at: Option<String>,
}

#[derive(Debug)]
struct ChangelogSection {
    version: Version,
    markdown: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    html_url: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
}

impl ReleaseNotes {
    /// Returns the bundled notes for this version and any skipped releases.
    #[must_use]
    pub fn bundled(version: &str, last_seen_version: Option<&str>) -> Self {
        Self::from_changelog(EMBEDDED_CHANGELOG, version, last_seen_version)
    }

    fn from_changelog(changelog: &str, version: &str, last_seen_version: Option<&str>) -> Self {
        let current = Version::parse(version).ok();
        let previous = last_seen_version.and_then(|value| Version::parse(value).ok());
        let previous = match (&current, previous) {
            (Some(current), Some(previous)) if previous < *current => Some(previous),
            _ => None,
        };
        let sections = changelog_sections(changelog)
            .into_iter()
            .filter(|section| {
                current.as_ref().is_some_and(|current| {
                    section.version <= *current
                        && previous
                            .as_ref()
                            .map_or(section.version == *current, |previous| {
                                section.version > *previous
                            })
                })
            })
            .map(|section| section.markdown)
            .collect::<Vec<_>>();
        let release_url = format!("https://github.com/{REPOSITORY}/releases/tag/v{version}");
        let markdown = if sections.is_empty() {
            format!("## Red v{version}\n\n[Read the release notes on GitHub]({release_url}).")
        } else {
            sections.join("\n\n")
        };

        Self {
            version: version.to_string(),
            markdown,
            release_url,
            published_at: None,
        }
    }

    /// Reduces the full changelog to the most useful user-visible changes.
    #[must_use]
    pub fn highlights_markdown(&self) -> String {
        let commit_link = Regex::new(
            r"\s*\(\[[0-9a-f]{7,40}\]\(https://github\.com/[^)]+/commit/[0-9a-f]{7,40}\)\)",
        )
        .expect("release commit-link expression is valid");
        let mut sections = Vec::new();

        for (heading, label) in [
            ("Features", "New features"),
            ("Performance", "Faster and smoother"),
            ("Bug Fixes", "Fixes"),
        ] {
            let mut in_section = false;
            let mut changes = Vec::new();
            for line in self.markdown.lines() {
                if let Some(section) = line.strip_prefix("### ") {
                    in_section = section.trim().eq_ignore_ascii_case(heading);
                    continue;
                }
                if line.starts_with("## ") {
                    in_section = false;
                    continue;
                }
                if in_section && line.starts_with("- ") {
                    let cleaned = commit_link.replace_all(line, "").trim().to_string();
                    changes.push(cleaned);
                    if changes.len() == MAX_HIGHLIGHTS_PER_SECTION {
                        break;
                    }
                }
            }
            if !changes.is_empty() {
                sections.push(format!("## {label}\n\n{}", changes.join("\n")));
            }
        }

        if sections.is_empty() {
            self.markdown.clone()
        } else {
            sections.join("\n\n")
        }
    }

    /// Retrieves and validates the published release for the running version.
    pub async fn fetch(version: &str, fallback: &Self) -> Result<Self> {
        let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/tags/v{version}");
        let client = reqwest::Client::builder()
            .timeout(RELEASE_REQUEST_TIMEOUT)
            .user_agent(format!("red/{version}"))
            .build()
            .context("failed to configure the GitHub release client")?;
        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .context("failed to retrieve the GitHub release")?
            .error_for_status()
            .context("GitHub rejected the release request")?;

        if response
            .content_length()
            .is_some_and(|length| length > MAX_RELEASE_NOTES_BYTES as u64)
        {
            anyhow::bail!("GitHub release notes exceed the permitted response size");
        }

        let bytes = response
            .bytes()
            .await
            .context("failed to read GitHub release notes")?;
        anyhow::ensure!(
            bytes.len() <= MAX_RELEASE_NOTES_BYTES,
            "GitHub release notes exceed the permitted response size"
        );
        Self::from_github_json(&bytes, version, fallback)
    }

    fn from_github_json(bytes: &[u8], version: &str, fallback: &Self) -> Result<Self> {
        let release: GitHubRelease =
            serde_json::from_slice(bytes).context("failed to decode GitHub release metadata")?;
        let expected_tag = format!("v{version}");
        anyhow::ensure!(
            release.tag_name == expected_tag,
            "GitHub returned release {}, expected {expected_tag}",
            release.tag_name
        );
        anyhow::ensure!(
            !release.draft,
            "the requested GitHub release is unpublished"
        );
        let markdown = release
            .body
            .as_deref()
            .map(strip_release_boilerplate)
            .filter(|body| !body.trim().is_empty())
            .ok_or_else(|| anyhow!("the GitHub release contains no changelog"))?;

        let skipped_releases = trailing_release_sections(&fallback.markdown);
        let markdown = if skipped_releases.is_empty() {
            markdown
        } else {
            format!("{markdown}\n\n{skipped_releases}")
        };

        let published_at = release.published_at.and_then(|value| {
            chrono::DateTime::parse_from_rfc3339(&value)
                .ok()
                .map(|date| date.format("%b %e, %Y").to_string().replace("  ", " "))
        });

        Ok(Self {
            version: version.to_string(),
            markdown,
            release_url: release
                .html_url
                .unwrap_or_else(|| fallback.release_url.clone()),
            published_at,
        })
    }
}

fn changelog_sections(changelog: &str) -> Vec<ChangelogSection> {
    let mut sections = Vec::new();
    let mut current_version = None;
    let mut current_lines = Vec::new();

    for line in changelog.lines() {
        if let Some(version) = release_heading_version(line) {
            if let Some(previous) = current_version.replace(version) {
                sections.push(ChangelogSection {
                    version: previous,
                    markdown: current_lines.join("\n").trim().to_string(),
                });
                current_lines.clear();
            }
            current_lines.push(line.to_string());
        } else if current_version.is_some() {
            current_lines.push(line.to_string());
        }
    }

    if let Some(version) = current_version {
        sections.push(ChangelogSection {
            version,
            markdown: current_lines.join("\n").trim().to_string(),
        });
    }

    sections
}

fn release_heading_version(line: &str) -> Option<Version> {
    let heading = line.strip_prefix("## [")?;
    let (version, _) = heading.split_once(']')?;
    Version::parse(version.trim_start_matches('v')).ok()
}

fn strip_release_boilerplate(markdown: &str) -> String {
    let mut lines = Vec::new();
    for line in markdown.lines() {
        if matches!(
            line.trim(),
            "## Installation" | "## Checksums" | "## Full Changelog"
        ) {
            break;
        }
        lines.push(line);
    }
    lines.join("\n").trim().to_string()
}

fn trailing_release_sections(markdown: &str) -> String {
    let mut headings_seen = 0_usize;
    let mut lines = Vec::new();
    for line in markdown.lines() {
        if release_heading_version(line).is_some() {
            headings_seen += 1;
        }
        if headings_seen >= 2 {
            lines.push(line);
        }
    }
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGELOG: &str = "# Changelog\n\n## [0.5.0](https://example.test/0.5)\n\n### Features\n\n- **editor:** New thing ([#2](https://github.com/codersauce/red/issues/2)) ([abcdef0](https://github.com/codersauce/red/commit/abcdef0123456789))\n\n### Bug Fixes\n\n- Fix old thing\n\n## [0.4.0](https://example.test/0.4)\n\n### Features\n\n- Previous thing\n\n## [0.3.0](https://example.test/0.3)\n\n### Features\n\n- Old thing\n";

    #[test]
    fn bundles_only_the_installed_version_for_a_first_launch() {
        let notes = ReleaseNotes::from_changelog(CHANGELOG, "0.5.0", None);

        assert!(notes.markdown.contains("New thing"));
        assert!(!notes.markdown.contains("Previous thing"));
    }

    #[test]
    fn includes_every_version_skipped_since_the_previous_seen_release() {
        let notes = ReleaseNotes::from_changelog(CHANGELOG, "0.5.0", Some("0.3.0"));

        assert!(notes.markdown.contains("New thing"));
        assert!(notes.markdown.contains("Previous thing"));
        assert!(!notes.markdown.contains("Old thing"));
    }

    #[test]
    fn downgrades_show_only_the_running_release() {
        let notes = ReleaseNotes::from_changelog(CHANGELOG, "0.4.0", Some("0.5.0"));

        assert!(notes.markdown.contains("Previous thing"));
        assert!(!notes.markdown.contains("Old thing"));
    }

    #[test]
    fn unavailable_embedded_versions_still_offer_a_release_link() {
        let notes = ReleaseNotes::from_changelog(CHANGELOG, "0.9.0", None);

        assert!(notes.markdown.contains("Red v0.9.0"));
        assert!(notes.markdown.contains("releases/tag/v0.9.0"));
    }

    #[test]
    fn highlights_keep_issue_links_and_remove_commit_noise() {
        let notes = ReleaseNotes::from_changelog(CHANGELOG, "0.5.0", None);
        let highlights = notes.highlights_markdown();

        assert!(highlights.contains("## New features"));
        assert!(highlights.contains("## Fixes"));
        assert!(highlights.contains("issues/2"));
        assert!(!highlights.contains("commit/abcdef"));
    }

    #[test]
    fn github_release_replaces_current_notes_and_keeps_skipped_versions() {
        let fallback = ReleaseNotes::from_changelog(CHANGELOG, "0.5.0", Some("0.3.0"));
        let payload = serde_json::json!({
            "tag_name": "v0.5.0",
            "body": "## [0.5.0](https://example.test)\n\n### Features\n\n- Published improvement\n\n## Installation\n\nDo not show this.",
            "html_url": "https://github.com/codersauce/red/releases/tag/v0.5.0",
            "published_at": "2026-08-13T21:03:28Z",
            "draft": false
        });

        let notes = ReleaseNotes::from_github_json(
            &serde_json::to_vec(&payload).unwrap(),
            "0.5.0",
            &fallback,
        )
        .unwrap();

        assert!(notes.markdown.contains("Published improvement"));
        assert!(notes.markdown.contains("Previous thing"));
        assert!(!notes.markdown.contains("Do not show this"));
        assert_eq!(notes.published_at.as_deref(), Some("Aug 13, 2026"));
    }

    #[test]
    fn rejects_wrong_or_unpublished_github_releases() {
        let fallback = ReleaseNotes::from_changelog(CHANGELOG, "0.5.0", None);
        let wrong = serde_json::json!({
            "tag_name": "v0.6.0",
            "body": "### Features\n- Wrong version",
            "draft": false
        });
        let draft = serde_json::json!({
            "tag_name": "v0.5.0",
            "body": "### Features\n- Unpublished",
            "draft": true
        });

        assert!(ReleaseNotes::from_github_json(
            &serde_json::to_vec(&wrong).unwrap(),
            "0.5.0",
            &fallback,
        )
        .is_err());
        assert!(ReleaseNotes::from_github_json(
            &serde_json::to_vec(&draft).unwrap(),
            "0.5.0",
            &fallback,
        )
        .is_err());
    }

    #[test]
    fn actual_embedded_changelog_contains_the_current_package_release() {
        let notes = ReleaseNotes::bundled(env!("CARGO_PKG_VERSION"), None);

        assert!(notes
            .markdown
            .contains(&format!("## [{}]", env!("CARGO_PKG_VERSION"))));
    }
}
