//! Updating the Android app installed from outside Google Play: the signed
//! manifest of its newest version, read as the desktop reads its own
//! (`src/update`).
//!
//! The file on `dl.shellrs.com` is the desktop's envelope: the manifest's
//! exact text next to its minisign signature, signed with the desktop's keys
//! by the same release workflow. Its trusted comment is
//! `shellrs-android-manifest <channel> <version>`, its own prefix, so that a
//! desktop manifest is never taken for the app's. Nothing in the manifest is
//! believed before the signature checks out; the app then trusts the
//! package by the size and SHA-256 it gives, and Android installs it only
//! over an app signed by the same key.
//!
//! The format only ever gains fields, as the desktop's does. The app does
//! the downloads and the installing; this checks what it downloaded.

use std::collections::BTreeMap;
use std::fmt;

use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;

/// Where each channel's signed manifest for the Android app lives. Every
/// copy ever shipped asks here, so the address never changes.
const MANIFEST_URL: &str = "https://dl.shellrs.com/update/android/v1/{channel}.json";

/// The key of the Android app's package in a manifest's assets.
const PLATFORM: &str = "android";

/// The newest manifest format this build reads.
const SCHEMA: u32 = 1;

/// What a signature's trusted comment starts with.
const COMMENT_PREFIX: &str = "shellrs-android-manifest ";

/// The desktop's keys (`src/update/build_info.rs`): the release workflow
/// signs both apps' manifests. Rotating a key means shipping an app that
/// knows the new one before signing with it.
const TRUSTED_KEYS: &[&str] = &[
    // In use (key id CD239E668A535CB0).
    "RWSwXFOKZp4jzaKmM9EkpY3iIyI0vMzi4EhFDlXwGy+XYiaHIWZY4ELa",
    // Spare, kept offline (key id F3B9F08DE80AF399).
    "RWSZ8wrojfC584RC+h7Frcp/pDW0pd7TaB4pO6bTVVOgSj6mo1KFs0sh",
];

/// Which stream of releases the app follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum UpdateChannel {
    Stable,
    Beta,
}

impl UpdateChannel {
    /// The name in the manifest's address and in its signature.
    fn key(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

/// The package to update with: where to get it and how to recognise it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct UpdatePackage {
    /// Tried in order, so a mirror can follow the main address.
    pub urls: Vec<String>,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

/// A newer version the app can update itself to.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct UpdateRelease {
    pub version: String,
    /// RFC 3339, e.g. `2026-10-20T08:00:00Z`.
    pub published_at: Option<String>,
    /// What changed, in Markdown.
    pub notes: String,
    pub notes_url: Option<String>,
    pub asset: UpdatePackage,
}

/// What the manifest means for this copy of the app.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum UpdateOffer {
    /// Nothing newer, or not yet for this device.
    UpToDate,
    Update {
        release: UpdateRelease,
    },
    /// A newer version this copy cannot install itself: the person
    /// downloads it from the website.
    Manual {
        version: String,
    },
}

/// Why a manifest was not believed.
#[derive(Debug, PartialEq, Eq, uniffi::Error)]
pub enum UpdateFailure {
    /// Not a manifest, or not one this build can read.
    BadManifest { detail: String },
    /// Not signed by a trusted key, or signed for another channel or
    /// version than it says.
    BadSignature,
    /// This copy's own version is not a semantic version.
    BadVersion { version: String },
}

impl fmt::Display for UpdateFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadManifest { detail } => write!(f, "bad manifest: {detail}"),
            Self::BadSignature => f.write_str("bad signature"),
            Self::BadVersion { version } => write!(f, "bad version: {version}"),
        }
    }
}

impl std::error::Error for UpdateFailure {}

/// The address of `channel`'s manifest.
#[uniffi::export]
pub fn android_update_manifest_url(channel: UpdateChannel) -> String {
    MANIFEST_URL.replace("{channel}", channel.key())
}

/// What the manifest `envelope`, as downloaded, offers a copy of the app at
/// version `current` on `channel`. `draw` is the device's own number in
/// `0..1` for staged rollouts, drawn once and kept. `extra_keys` are trusted
/// besides the built-in ones, minisign public keys in base64: only a debug
/// build passes any, to try updating against a server of its own.
#[uniffi::export]
pub fn check_android_update(
    envelope: Vec<u8>,
    channel: UpdateChannel,
    current: String,
    draw: f64,
    extra_keys: Vec<String>,
) -> Result<UpdateOffer, UpdateFailure> {
    let current = Version::parse(current.trim()).map_err(|_| UpdateFailure::BadVersion {
        version: current.clone(),
    })?;
    let keys: Vec<PublicKey> = TRUSTED_KEYS
        .iter()
        .copied()
        .chain(extra_keys.iter().map(String::as_str))
        .filter_map(|key| PublicKey::from_base64(key.trim()).ok())
        .collect();
    Ok(match open(&envelope, channel, &keys)? {
        Opened::Manifest(manifest) => manifest.offer(&current, draw),
        Opened::NewerFormat { version } if version > current => UpdateOffer::Manual {
            version: version.to_string(),
        },
        Opened::NewerFormat { .. } => UpdateOffer::UpToDate,
    })
}

/// The file as served: the manifest's text and its signature.
#[derive(Deserialize)]
struct Envelope {
    manifest: String,
    signature: String,
}

/// The manifest, as the desktop's: unknown fields are ignored.
#[derive(Deserialize)]
struct Manifest {
    #[allow(dead_code)]
    schema: u32,
    channel: String,
    version: Version,
    #[serde(default)]
    published_at: Option<String>,
    /// The oldest version that may update itself to this one; older ones
    /// are sent to the website.
    #[serde(default)]
    minimum_version: Option<Version>,
    /// The share of devices offered this release, 0 to 1.
    #[serde(default = "everyone")]
    rollout: f64,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    notes_url: Option<String>,
    #[serde(default)]
    assets: BTreeMap<String, Asset>,
}

fn everyone() -> f64 {
    1.0
}

#[derive(Clone, Deserialize)]
struct Asset {
    urls: Vec<String>,
    size: u64,
    sha256: String,
}

/// Read before the rest, so that a newer format is told from a broken file.
#[derive(Deserialize)]
struct SchemaOnly {
    schema: u32,
}

enum Opened {
    Manifest(Manifest),
    /// Signed, but in a format newer than this build reads: its signed
    /// version still says whether there is something to download.
    NewerFormat {
        version: Version,
    },
}

/// Check the envelope's signature, then open the manifest inside.
fn open(bytes: &[u8], channel: UpdateChannel, keys: &[PublicKey]) -> Result<Opened, UpdateFailure> {
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|error| UpdateFailure::BadManifest {
            detail: error.to_string(),
        })?;
    let signature =
        Signature::decode(&envelope.signature).map_err(|_| UpdateFailure::BadSignature)?;
    // Prehashed signatures only, as minisign makes them and the desktop takes them.
    let signed = keys.iter().any(|key| {
        key.verify(envelope.manifest.as_bytes(), &signature, false)
            .is_ok()
    });
    if !signed {
        return Err(UpdateFailure::BadSignature);
    }
    let version =
        signed_version(signature.trusted_comment(), channel).ok_or(UpdateFailure::BadSignature)?;
    let bad = |error: serde_json::Error| UpdateFailure::BadManifest {
        detail: error.to_string(),
    };
    let SchemaOnly { schema } = serde_json::from_str(&envelope.manifest).map_err(bad)?;
    if schema > SCHEMA {
        return Ok(Opened::NewerFormat { version });
    }
    let manifest: Manifest = serde_json::from_str(&envelope.manifest).map_err(bad)?;
    // What the signature vouches for is what the manifest says.
    if manifest.channel != channel.key() || manifest.version != version {
        return Err(UpdateFailure::BadSignature);
    }
    Ok(Opened::Manifest(manifest))
}

/// The version a trusted comment vouches for on `channel`.
fn signed_version(comment: &str, channel: UpdateChannel) -> Option<Version> {
    let rest = comment.strip_prefix(COMMENT_PREFIX)?;
    let (signed_channel, version) = rest.split_once(' ')?;
    if signed_channel != channel.key() {
        return None;
    }
    Version::parse(version.trim()).ok()
}

impl Manifest {
    /// As the desktop's `Manifest::offer`, for the Android package.
    fn offer(&self, current: &Version, draw: f64) -> UpdateOffer {
        if self.version <= *current || draw >= self.rollout {
            return UpdateOffer::UpToDate;
        }
        let too_old = self
            .minimum_version
            .as_ref()
            .is_some_and(|minimum| current < minimum);
        match self.assets.get(PLATFORM) {
            Some(asset) if !too_old => UpdateOffer::Update {
                release: UpdateRelease {
                    version: self.version.to_string(),
                    published_at: self.published_at.clone(),
                    notes: self.notes.clone(),
                    notes_url: self.notes_url.clone(),
                    asset: UpdatePackage {
                        urls: asset.urls.clone(),
                        size: asset.size,
                        sha256: asset.sha256.trim().to_ascii_lowercase(),
                    },
                },
            },
            Some(_) => UpdateOffer::Manual {
                version: self.version.to_string(),
            },
            None => UpdateOffer::UpToDate,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use minisign::KeyPair;

    use super::*;

    fn test_key() -> (KeyPair, String) {
        let pair = KeyPair::generate_unencrypted_keypair().unwrap();
        let public = pair.pk.to_base64();
        (pair, public)
    }

    fn manifest_text(channel: &str, version: &str, extra: &str) -> String {
        format!(
            r####"{{
              "schema": 1, "channel": "{channel}", "version": "{version}",
              "published_at": "2026-11-20T08:00:00Z",
              "notes": "### 新增\n- 自动更新",
              "assets": {{
                "android": {{ "urls": ["https://dl.shellrs.com/a.apk", "https://github.com/a.apk"], "size": 3, "sha256": "AB" }}
              }},
              "something_added_later": {{ "ignored": true }}{extra}
            }}"####
        )
    }

    fn envelope(pair: &KeyPair, manifest: &str, comment: &str) -> Vec<u8> {
        let signature = minisign::sign(
            Some(&pair.pk),
            &pair.sk,
            Cursor::new(manifest.as_bytes()),
            Some(comment),
            None,
        )
        .unwrap()
        .into_string();
        serde_json::to_vec(&serde_json::json!({ "manifest": manifest, "signature": signature }))
            .unwrap()
    }

    fn check(bytes: Vec<u8>, current: &str, public: &str) -> Result<UpdateOffer, UpdateFailure> {
        check_android_update(
            bytes,
            UpdateChannel::Stable,
            current.into(),
            0.5,
            vec![public.into()],
        )
    }

    #[test]
    fn a_signed_manifest_offers_its_package() {
        let (pair, public) = test_key();
        let bytes = envelope(
            &pair,
            &manifest_text("stable", "0.2.0", ""),
            "shellrs-android-manifest stable 0.2.0",
        );
        let Ok(UpdateOffer::Update { release }) = check(bytes.clone(), "0.1.0", &public) else {
            panic!("a newer signed version is offered");
        };
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.asset.urls.len(), 2);
        assert_eq!(release.asset.size, 3);
        assert_eq!(release.asset.sha256, "ab");
        assert_eq!(release.notes, "### 新增\n- 自动更新");
        // The same version, or a newer one already: nothing to do.
        assert_eq!(
            check(bytes.clone(), "0.2.0", &public),
            Ok(UpdateOffer::UpToDate)
        );
        assert_eq!(check(bytes, "0.3.0", &public), Ok(UpdateOffer::UpToDate));
    }

    #[test]
    fn what_is_not_signed_by_a_trusted_key_is_rejected() {
        let (pair, public) = test_key();
        let (_, other) = test_key();
        let comment = "shellrs-android-manifest stable 0.2.0";
        let bytes = envelope(&pair, &manifest_text("stable", "0.2.0", ""), comment);
        assert_eq!(
            check(bytes, "0.1.0", &other),
            Err(UpdateFailure::BadSignature)
        );
        // Changed after signing.
        let tampered: serde_json::Value = serde_json::from_slice(&envelope(
            &pair,
            &manifest_text("stable", "0.2.0", ""),
            comment,
        ))
        .unwrap();
        let mut tampered = tampered;
        tampered["manifest"] = manifest_text("stable", "0.2.0", "")
            .replace("dl.shellrs.com", "evil.example")
            .into();
        let bytes = serde_json::to_vec(&tampered).unwrap();
        assert_eq!(
            check(bytes, "0.1.0", &public),
            Err(UpdateFailure::BadSignature)
        );
        // Not an envelope at all.
        assert!(matches!(
            check(b"<html>".to_vec(), "0.1.0", &public),
            Err(UpdateFailure::BadManifest { .. })
        ));
        // The built-in keys alone do not trust a test key.
        let bytes = envelope(&pair, &manifest_text("stable", "0.2.0", ""), comment);
        assert_eq!(
            check_android_update(bytes, UpdateChannel::Stable, "0.1.0".into(), 0.5, vec![]),
            Err(UpdateFailure::BadSignature)
        );
    }

    #[test]
    fn a_signature_vouches_for_one_channel_version_and_app() {
        let (pair, public) = test_key();
        let text = manifest_text("stable", "0.2.0", "");
        // An old manifest, genuinely signed, served as a newer one.
        let bytes = envelope(&pair, &text, "shellrs-android-manifest stable 0.1.5");
        assert_eq!(
            check(bytes, "0.1.0", &public),
            Err(UpdateFailure::BadSignature)
        );
        // A beta manifest served on the stable channel.
        let beta = manifest_text("beta", "0.2.0", "");
        let bytes = envelope(&pair, &beta, "shellrs-android-manifest beta 0.2.0");
        assert_eq!(
            check(bytes, "0.1.0", &public),
            Err(UpdateFailure::BadSignature)
        );
        // The desktop's manifest, signed with the same key.
        let bytes = envelope(&pair, &text, "shellrs-manifest stable 0.2.0");
        assert_eq!(
            check(bytes, "0.1.0", &public),
            Err(UpdateFailure::BadSignature)
        );
    }

    #[test]
    fn rollout_and_minimum_version_decide_what_is_offered() {
        let (pair, public) = test_key();
        let comment = "shellrs-android-manifest stable 0.2.0";
        // Half the devices: this one drew 0.5.
        let half = envelope(
            &pair,
            &manifest_text("stable", "0.2.0", r#", "rollout": 0.5"#),
            comment,
        );
        assert_eq!(
            check(half.clone(), "0.1.0", &public),
            Ok(UpdateOffer::UpToDate)
        );
        assert!(matches!(
            check_android_update(
                half,
                UpdateChannel::Stable,
                "0.1.0".into(),
                0.2,
                vec![public.clone()]
            ),
            Ok(UpdateOffer::Update { .. })
        ));
        // Too old to update itself: to the website.
        let minimum = envelope(
            &pair,
            &manifest_text("stable", "0.2.0", r#", "minimum_version": "0.1.5""#),
            comment,
        );
        assert_eq!(
            check(minimum, "0.1.0", &public),
            Ok(UpdateOffer::Manual {
                version: "0.2.0".into()
            })
        );
        // A manifest without the Android package offers nothing.
        let desktop_only =
            manifest_text("stable", "0.2.0", "").replace("\"android\"", "\"macos-aarch64\"");
        let bytes = envelope(&pair, &desktop_only, comment);
        assert_eq!(check(bytes, "0.1.0", &public), Ok(UpdateOffer::UpToDate));
    }

    #[test]
    fn a_newer_format_still_names_its_version() {
        let (pair, public) = test_key();
        let text = r#"{ "schema": 9, "everything": "different" }"#;
        let bytes = envelope(&pair, text, "shellrs-android-manifest stable 0.3.0");
        assert_eq!(
            check(bytes.clone(), "0.1.0", &public),
            Ok(UpdateOffer::Manual {
                version: "0.3.0".into()
            })
        );
        assert_eq!(check(bytes, "0.3.0", &public), Ok(UpdateOffer::UpToDate));
    }

    #[test]
    fn the_address_and_the_version_are_checked() {
        assert_eq!(
            android_update_manifest_url(UpdateChannel::Stable),
            "https://dl.shellrs.com/update/android/v1/stable.json"
        );
        assert_eq!(
            android_update_manifest_url(UpdateChannel::Beta),
            "https://dl.shellrs.com/update/android/v1/beta.json"
        );
        assert_eq!(
            check_android_update(vec![], UpdateChannel::Stable, "1.0".into(), 0.5, vec![]),
            Err(UpdateFailure::BadVersion {
                version: "1.0".into()
            })
        );
    }

    /// The keys are the desktop's, which the release workflow signs with.
    #[test]
    fn the_keys_are_the_desktops() {
        let desktop = include_str!("../../../src/update/build_info.rs");
        for key in TRUSTED_KEYS {
            assert!(
                desktop.contains(key),
                "{key} is not among the desktop's keys"
            );
            assert!(PublicKey::from_base64(key).is_ok());
        }
    }
}
