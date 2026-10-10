//! Trusting what the update server sends.
//!
//! The manifest is signed as a whole: its notes are shown with clickable
//! links and its addresses are where the next ShellRS comes from, so none of
//! it is believed before the signature checks out. The signature's trusted
//! comment, `shellrs-manifest <channel> <version>`, is covered by the
//! signature too; matching it against the manifest stops an old, genuinely
//! signed manifest from being served as a newer one, and a beta manifest
//! from being served on the stable channel. A package is then trusted by its
//! size and SHA-256, which the signed manifest gives.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use minisign_verify::{PublicKey, Signature};
use semver::Version;
use sha2::{Digest as _, Sha256};

use super::build_info::{Channel, TRUSTED_KEYS};
use super::error::UpdateError;
use super::manifest::{self, Asset, Envelope, Manifest};

/// The keys a manifest may be signed with.
#[derive(Clone, Debug, Default)]
pub struct TrustedKeys(Vec<PublicKey>);

impl TrustedKeys {
    /// Keys in minisign's base64 form, the second line of a `.pub` file.
    pub fn new<'a>(keys: impl IntoIterator<Item = &'a str>) -> Self {
        Self(
            keys.into_iter()
                .filter_map(|key| PublicKey::from_base64(key.trim()).ok())
                .collect(),
        )
    }

    /// The keys built into this ShellRS. A debug build also trusts the key
    /// in `SHELLRS_UPDATE_PUBLIC_KEY`, to try updating against a local
    /// server; a release build trusts only the keys it was built with.
    pub fn builtin() -> Self {
        let test_key = cfg!(debug_assertions)
            .then(|| std::env::var("SHELLRS_UPDATE_PUBLIC_KEY").ok())
            .flatten();
        Self::new(TRUSTED_KEYS.iter().copied().chain(test_key.as_deref()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A manifest whose signature checked out.
#[derive(Clone, Debug, PartialEq)]
pub enum Opened {
    Manifest(Manifest),
    /// Signed, but in a format newer than this build reads. The signed
    /// version still says whether there is something to download by hand.
    NewerFormat {
        version: Version,
    },
}

/// Check the envelope's signature and open the manifest inside.
pub fn open_envelope(
    bytes: &[u8],
    channel: Channel,
    keys: &TrustedKeys,
) -> Result<Opened, UpdateError> {
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|error| UpdateError::BadManifest(error.to_string()))?;
    let signature =
        Signature::decode(&envelope.signature).map_err(|_| UpdateError::BadSignature)?;
    let signed = keys.0.iter().any(|key| {
        key.verify(envelope.manifest.as_bytes(), &signature, false)
            .is_ok()
    });
    if !signed {
        return Err(UpdateError::BadSignature);
    }
    let version =
        signed_version(signature.trusted_comment(), channel).ok_or(UpdateError::BadSignature)?;
    match manifest::parse(&envelope.manifest)
        .map_err(|error| UpdateError::BadManifest(error.to_string()))?
    {
        None => Ok(Opened::NewerFormat { version }),
        Some(manifest) if manifest.channel == channel && manifest.version == version => {
            Ok(Opened::Manifest(manifest))
        }
        Some(_) => Err(UpdateError::BadSignature),
    }
}

/// The version a trusted comment vouches for on `channel`.
fn signed_version(comment: &str, channel: Channel) -> Option<Version> {
    let rest = comment.strip_prefix("shellrs-manifest ")?;
    let (signed_channel, version) = rest.split_once(' ')?;
    if signed_channel != channel.key() {
        return None;
    }
    Version::parse(version.trim()).ok()
}

/// Whether the file at `path` is the package `asset` describes.
pub fn verify_file(path: &Path, asset: &Asset) -> Result<(), UpdateError> {
    if std::fs::metadata(path)?.len() != asset.size {
        return Err(UpdateError::Corrupt);
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest.eq_ignore_ascii_case(asset.sha256.trim()) {
        Ok(())
    } else {
        Err(UpdateError::Corrupt)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use minisign::KeyPair;

    use super::*;
    use crate::update::manifest::tests::manifest_text;

    /// A key pair for tests, and its public key as `TrustedKeys` takes it.
    pub(crate) fn test_key() -> (KeyPair, String) {
        let pair = KeyPair::generate_unencrypted_keypair().unwrap();
        let public = pair.pk.to_base64();
        (pair, public)
    }

    /// An envelope as the release workflow publishes it.
    pub(crate) fn envelope(pair: &KeyPair, manifest: &str, comment: &str) -> Vec<u8> {
        let signature = minisign::sign(
            Some(&pair.pk),
            &pair.sk,
            Cursor::new(manifest.as_bytes()),
            Some(comment),
            None,
        )
        .unwrap()
        .into_string();
        serde_json::to_vec(&serde_json::json!({
            "manifest": manifest,
            "signature": signature,
        }))
        .unwrap()
    }

    #[test]
    fn a_manifest_signed_by_either_trusted_key_is_accepted() {
        let (current, current_public) = test_key();
        let (spare, spare_public) = test_key();
        let keys = TrustedKeys::new([current_public.as_str(), spare_public.as_str()]);
        for pair in [&current, &spare] {
            let bytes = envelope(
                pair,
                &manifest_text("0.2.0"),
                "shellrs-manifest stable 0.2.0",
            );
            let Ok(Opened::Manifest(manifest)) = open_envelope(&bytes, Channel::Stable, &keys)
            else {
                panic!("a manifest signed by a trusted key opens");
            };
            assert_eq!(manifest.version, Version::new(0, 2, 0));
        }
    }

    #[test]
    fn a_tampered_or_unknown_key_manifest_is_rejected() {
        let (pair, public) = test_key();
        let keys = TrustedKeys::new([public.as_str()]);
        let signed = envelope(
            &pair,
            &manifest_text("0.2.0"),
            "shellrs-manifest stable 0.2.0",
        );
        let tampered = String::from_utf8(signed)
            .unwrap()
            .replace("a.zip", "evil.zip")
            .into_bytes();
        assert_eq!(
            open_envelope(&tampered, Channel::Stable, &keys),
            Err(UpdateError::BadSignature)
        );

        let (stranger, _) = test_key();
        let unknown = envelope(
            &stranger,
            &manifest_text("0.2.0"),
            "shellrs-manifest stable 0.2.0",
        );
        assert_eq!(
            open_envelope(&unknown, Channel::Stable, &keys),
            Err(UpdateError::BadSignature)
        );
        assert_eq!(
            open_envelope(&unknown, Channel::Stable, &TrustedKeys::default()),
            Err(UpdateError::BadSignature)
        );
        assert!(matches!(
            open_envelope(b"<html>", Channel::Stable, &keys),
            Err(UpdateError::BadManifest(_))
        ));
    }

    #[test]
    fn an_old_manifest_signed_as_a_newer_version_is_rejected() {
        let (pair, public) = test_key();
        let keys = TrustedKeys::new([public.as_str()]);
        let replayed = envelope(
            &pair,
            &manifest_text("0.2.0"),
            "shellrs-manifest stable 0.9.0",
        );
        assert_eq!(
            open_envelope(&replayed, Channel::Stable, &keys),
            Err(UpdateError::BadSignature)
        );
    }

    #[test]
    fn a_beta_manifest_served_as_stable_is_rejected() {
        let (pair, public) = test_key();
        let keys = TrustedKeys::new([public.as_str()]);
        let beta = manifest_text("0.3.0-beta.1").replace("\"stable\"", "\"beta\"");
        let bytes = envelope(&pair, &beta, "shellrs-manifest beta 0.3.0-beta.1");
        assert_eq!(
            open_envelope(&bytes, Channel::Stable, &keys),
            Err(UpdateError::BadSignature)
        );
        assert!(matches!(
            open_envelope(&bytes, Channel::Beta, &keys),
            Ok(Opened::Manifest(_))
        ));
    }

    #[test]
    fn a_signed_manifest_in_a_newer_format_still_names_its_version() {
        let (pair, public) = test_key();
        let keys = TrustedKeys::new([public.as_str()]);
        let bytes = envelope(&pair, r#"{"schema": 2}"#, "shellrs-manifest stable 3.0.0");
        assert_eq!(
            open_envelope(&bytes, Channel::Stable, &keys),
            Ok(Opened::NewerFormat {
                version: Version::new(3, 0, 0)
            })
        );
    }

    #[test]
    fn a_download_of_the_wrong_size_or_hash_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("package");
        std::fs::write(&path, b"abc").unwrap();
        let asset = |size, sha256: &str| Asset {
            urls: Vec::new(),
            size,
            sha256: sha256.into(),
        };
        let sha_of_abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(verify_file(&path, &asset(3, sha_of_abc)), Ok(()));
        assert_eq!(
            verify_file(&path, &asset(3, &sha_of_abc.to_uppercase())),
            Ok(())
        );
        assert_eq!(
            verify_file(&path, &asset(4, sha_of_abc)),
            Err(UpdateError::Corrupt)
        );
        assert_eq!(
            verify_file(&path, &asset(3, &"0".repeat(64))),
            Err(UpdateError::Corrupt)
        );
    }

    /// The release workflow's `packaging/make-manifest.sh` and this module
    /// agree on the envelope. Needs the `minisign` and `jq` commands, so it
    /// passes quietly where they are not installed.
    #[test]
    fn the_release_script_signs_what_the_client_accepts() {
        let have = |tool: &str| {
            let _forks = crate::testing::no_forks();
            std::process::Command::new(tool)
                .arg(if tool == "jq" { "--version" } else { "-v" })
                .output()
                .is_ok_and(|output| output.status.success())
        };
        if !have("minisign") || !have("jq") {
            eprintln!("skipped: minisign or jq is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (public, secret) = (dir.path().join("key.pub"), dir.path().join("key.sec"));
        let forks = crate::testing::no_forks();
        let generated = std::process::Command::new("minisign")
            .args(["-G", "-W", "-p"])
            .arg(&public)
            .arg("-s")
            .arg(&secret)
            .output()
            .unwrap();
        assert!(generated.status.success(), "{generated:?}");
        let dist = dir.path().join("dist");
        std::fs::create_dir(&dist).unwrap();
        for name in [
            "ShellRS-0.2.0-macos-aarch64.app.zip",
            "ShellRS-0.2.0-macos-x86_64.app.zip",
            "ShellRS-0.2.0-windows-x86_64-setup.exe",
            "ShellRS-0.2.0-linux-x86_64.AppImage",
        ] {
            std::fs::write(dist.join(name), name).unwrap();
        }
        let notes = dir.path().join("notes.md");
        std::fs::write(&notes, "### 新增\n\n- 在线升级\n").unwrap();
        let out = dir.path().join("stable.json");
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/make-manifest.sh");
        let made = std::process::Command::new("bash")
            .arg(script)
            .args(["0.2.0", "stable"])
            .arg(&notes)
            .arg(&dist)
            .arg(&out)
            .env("MINISIGN_KEY_FILE", &secret)
            .env_remove("MINISIGN_PASSWORD")
            // As the release workflow sets it.
            .env(
                "SHELLRS_MIRROR_BASE",
                "https://github.com/since2006/shell-rs/releases/download/v0.2.0",
            )
            .output()
            .unwrap();
        drop(forks);
        assert!(made.status.success(), "{made:?}");

        let public = std::fs::read_to_string(&public).unwrap();
        let keys = TrustedKeys::new(public.lines().nth(1));
        let bytes = std::fs::read(&out).unwrap();
        let Ok(Opened::Manifest(manifest)) = open_envelope(&bytes, Channel::Stable, &keys) else {
            panic!("the client rejects the script's manifest");
        };
        assert_eq!(manifest.version, Version::new(0, 2, 0));
        assert_eq!(manifest.notes, "### 新增\n\n- 在线升级\n");
        for arch in ["aarch64", "x86_64"] {
            let key = format!("macos-{arch}");
            let name = format!("ShellRS-0.2.0-{key}.app.zip");
            let asset = &manifest.assets[&key];
            // Each architecture has its own package and both download sources.
            assert_eq!(
                asset.urls,
                [
                    format!("https://dl.shellrs.com/releases/0.2.0/{name}"),
                    format!(
                        "https://github.com/since2006/shell-rs/releases/download/v0.2.0/{name}"
                    ),
                ]
            );
            assert_eq!(verify_file(&dist.join(&name), asset), Ok(()));
            assert_eq!(
                manifest.installers[&key],
                format!("https://dl.shellrs.com/releases/0.2.0/ShellRS-0.2.0-{key}.dmg")
            );
        }
        assert_ne!(
            manifest.assets["macos-aarch64"].sha256,
            manifest.assets["macos-x86_64"].sha256
        );
        assert_eq!(
            verify_file(
                &dist.join("ShellRS-0.2.0-macos-x86_64.app.zip"),
                &manifest.assets["macos-aarch64"],
            ),
            Err(UpdateError::Corrupt)
        );
        let package = dist.join("ShellRS-0.2.0-linux-x86_64.AppImage");
        assert_eq!(
            verify_file(&package, &manifest.assets["linux-x86_64"]),
            Ok(())
        );
        assert!(
            manifest.installers["windows-x86_64"].ends_with("windows-x86_64-setup.exe"),
            "{:?}",
            manifest.installers
        );
        assert_eq!(
            open_envelope(&bytes, Channel::Beta, &keys),
            Err(UpdateError::BadSignature)
        );
    }

    #[test]
    fn the_builtin_keys_all_parse() {
        assert_eq!(TrustedKeys::builtin().0.len(), TRUSTED_KEYS.len());
    }
}
