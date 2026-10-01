//! The release manifest: what the newest version of a channel is and where
//! its packages are.
//!
//! The file on the server is an [`Envelope`]: the manifest's JSON as a
//! string next to its minisign signature. One file means one request and
//! no moment where a cached signature belongs to the previous manifest.
//! `verify::open_envelope` checks the signature before anything here reads
//! the manifest.
//!
//! Every ShellRS ever shipped reads this format, so fields are only ever
//! added. Unknown fields are ignored; a `schema` newer than [`SCHEMA`] means
//! the manifest needs a newer ShellRS than this one, which then points at the
//! download page instead of updating itself.

use std::collections::BTreeMap;

use semver::Version;
use serde::Deserialize;

use super::build_info::Channel;

/// The newest manifest format this build reads.
pub const SCHEMA: u32 = 1;

/// The file as served: the manifest's text and its signature.
#[derive(Debug, Deserialize)]
pub struct Envelope {
    pub manifest: String,
    pub signature: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub channel: Channel,
    pub version: Version,
    /// RFC 3339, e.g. `2026-10-20T08:00:00Z`.
    #[serde(default)]
    pub published_at: Option<String>,
    /// The oldest version that may update itself to this one. Older ones are
    /// sent to the download page: for when installing changes in a way an
    /// old updater cannot follow.
    #[serde(default)]
    pub minimum_version: Option<Version>,
    /// The share of machines offered this release, 0 to 1. Each machine
    /// draws its own number once and keeps it; nothing is reported back.
    #[serde(default = "everyone")]
    pub rollout: f64,
    /// What changed, in Markdown.
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub notes_url: Option<String>,
    /// The package each platform updates itself with, by platform key.
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
    /// What a person downloads to install by hand, by platform key.
    #[serde(default)]
    pub installers: BTreeMap<String, String>,
}

fn everyone() -> f64 {
    1.0
}

/// A package: where to get it and how to recognise it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Asset {
    /// Tried in order, so a mirror can follow the main address.
    pub urls: Vec<String>,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

/// A newer version this machine can move to.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    pub published_at: Option<String>,
    pub notes: String,
    pub notes_url: Option<String>,
    pub asset: Asset,
    pub installer: Option<String>,
}

impl Release {
    /// `2026-10-20`, from the publication time.
    pub fn published_on(&self) -> Option<&str> {
        self.published_at.as_deref().and_then(|at| at.get(..10))
    }
}

/// What a manifest means for this machine.
#[derive(Clone, Debug, PartialEq)]
pub enum Offer {
    /// Nothing newer for this platform, or not yet for this machine.
    UpToDate,
    /// A newer version to download and install.
    Update(Release),
    /// A newer version that this build cannot install itself; the person
    /// has to download it. Carries the installer's address when there is one.
    Manual {
        version: Version,
        installer: Option<String>,
    },
}

/// The schema of a manifest, read before the rest so that a format this
/// build does not know is recognised as such rather than as a broken file.
#[derive(Deserialize)]
struct SchemaOnly {
    schema: u32,
}

/// Read a manifest's text. `Ok(None)` is a format newer than this build.
pub fn parse(text: &str) -> Result<Option<Manifest>, serde_json::Error> {
    let SchemaOnly { schema } = serde_json::from_str(text)?;
    if schema > SCHEMA {
        return Ok(None);
    }
    serde_json::from_str(text).map(Some)
}

impl Manifest {
    /// What this manifest offers a machine running `current` on `platform`.
    /// `draw` is the machine's own number in `0..1` for staged rollouts.
    pub fn offer(&self, current: &Version, platform: &str, draw: f64) -> Offer {
        if self.version <= *current || draw >= self.rollout {
            return Offer::UpToDate;
        }
        let installer = self.installers.get(platform).cloned();
        let too_old = self
            .minimum_version
            .as_ref()
            .is_some_and(|minimum| current < minimum);
        match self.assets.get(platform) {
            Some(asset) if !too_old => Offer::Update(Release {
                version: self.version.clone(),
                published_at: self.published_at.clone(),
                notes: self.notes.clone(),
                notes_url: self.notes_url.clone(),
                asset: asset.clone(),
                installer,
            }),
            Some(_) => Offer::Manual {
                version: self.version.clone(),
                installer,
            },
            None => Offer::UpToDate,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn manifest_text(version: &str) -> String {
        format!(
            r####"{{
              "schema": 1, "channel": "stable", "version": "{version}",
              "published_at": "2026-10-20T08:00:00Z",
              "notes": "### 新增\n- 在线升级",
              "assets": {{
                "macos-aarch64": {{ "urls": ["https://dl.shellrs.com/a.zip"], "size": 3, "sha256": "ab" }},
                "linux-x86_64": {{ "urls": ["https://dl.shellrs.com/b.AppImage"], "size": 4, "sha256": "cd" }}
              }},
              "installers": {{ "macos-aarch64": "https://dl.shellrs.com/a.dmg" }},
              "something_added_later": {{ "ignored": true }}
            }}"####
        )
    }

    fn manifest(version: &str) -> Manifest {
        parse(&manifest_text(version)).unwrap().unwrap()
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_newer_version_is_offered_and_the_same_or_older_is_not() {
        let manifest = manifest("0.2.0");
        let Offer::Update(release) = manifest.offer(&v("0.1.0"), "macos-aarch64", 0.5) else {
            panic!("an update for 0.1.0");
        };
        assert_eq!(release.version, v("0.2.0"));
        assert_eq!(release.asset.urls, ["https://dl.shellrs.com/a.zip"]);
        assert_eq!(
            release.installer.as_deref(),
            Some("https://dl.shellrs.com/a.dmg")
        );
        assert_eq!(release.published_on(), Some("2026-10-20"));
        assert_eq!(
            manifest.offer(&v("0.2.0"), "macos-aarch64", 0.5),
            Offer::UpToDate
        );
        assert_eq!(
            manifest.offer(&v("0.3.0"), "macos-aarch64", 0.5),
            Offer::UpToDate
        );
    }

    #[test]
    fn a_prerelease_is_older_than_its_release() {
        let manifest = manifest("0.2.0");
        assert!(matches!(
            manifest.offer(&v("0.2.0-beta.2"), "macos-aarch64", 0.5),
            Offer::Update(_)
        ));
        let beta = manifest_text("0.2.0-beta.3");
        let beta = parse(&beta).unwrap().unwrap();
        assert!(matches!(
            beta.offer(&v("0.2.0-beta.2"), "linux-x86_64", 0.5),
            Offer::Update(_)
        ));
        assert_eq!(
            beta.offer(&v("0.2.0"), "linux-x86_64", 0.5),
            Offer::UpToDate
        );
    }

    #[test]
    fn a_manifest_without_this_platform_offers_nothing() {
        assert_eq!(
            manifest("0.2.0").offer(&v("0.1.0"), "windows-x86_64", 0.5),
            Offer::UpToDate
        );
    }

    #[test]
    fn a_draw_above_the_rollout_waits() {
        let mut manifest = manifest("0.2.0");
        manifest.rollout = 0.25;
        assert!(matches!(
            manifest.offer(&v("0.1.0"), "macos-aarch64", 0.1),
            Offer::Update(_)
        ));
        assert_eq!(
            manifest.offer(&v("0.1.0"), "macos-aarch64", 0.25),
            Offer::UpToDate
        );
        assert_eq!(
            manifest.offer(&v("0.1.0"), "macos-aarch64", 0.9),
            Offer::UpToDate
        );
    }

    #[test]
    fn a_version_below_the_minimum_is_sent_to_the_download_page() {
        let mut manifest = manifest("0.5.0");
        manifest.minimum_version = Some(v("0.3.0"));
        assert_eq!(
            manifest.offer(&v("0.2.0"), "macos-aarch64", 0.5),
            Offer::Manual {
                version: v("0.5.0"),
                installer: Some("https://dl.shellrs.com/a.dmg".into()),
            }
        );
        assert!(matches!(
            manifest.offer(&v("0.3.0"), "macos-aarch64", 0.5),
            Offer::Update(_)
        ));
    }

    #[test]
    fn a_manifest_with_unknown_fields_still_parses() {
        let manifest = manifest("0.2.0");
        assert_eq!(manifest.rollout, 1.0);
        assert_eq!(manifest.channel, Channel::Stable);
    }

    #[test]
    fn a_manifest_from_a_newer_format_is_recognised_as_such() {
        let text = r#"{ "schema": 2, "whatever": "changed shape" }"#;
        assert_eq!(parse(text).unwrap(), None);
        assert!(parse(r#"{ "schema": 1 }"#).is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn the_example_manifest_parses() {
        let text = include_str!("../../packaging/manifest.example.json");
        let manifest = parse(text).unwrap().expect("schema 1");
        for platform in [
            "macos-aarch64",
            "macos-x86_64",
            "windows-x86_64",
            "linux-x86_64",
        ] {
            assert!(manifest.assets.contains_key(platform), "{platform}");
            assert!(manifest.installers.contains_key(platform), "{platform}");
        }
    }
}
