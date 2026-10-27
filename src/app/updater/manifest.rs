//! The release manifest that `.github/release/publish-r2.sh` publishes to R2.
//!
//! `latest.json` names the newest stable release. Until its bytes verify, only
//! its version is read, to decide whether there is anything to fetch; the
//! release itself comes from those same bytes once
//! `releases/v<version>/manifest.json.sig` verifies them (Ed25519 over the
//! exact bytes, base64). See `.github/release/downloads.md`.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use serde::Deserialize;

/// The largest installer the updater downloads. Releases are about 15 MB.
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;

/// A stable release version, `MAJOR.MINOR.PATCH` exactly as the publisher's
/// tag check accepts it (no leading zeros, at most nine digits each).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut part = || -> Option<u32> {
            let digits = parts.next()?;
            let canonical = digits == "0"
                || (!digits.is_empty()
                    && digits.len() <= 9
                    && !digits.starts_with('0')
                    && digits.bytes().all(|byte| byte.is_ascii_digit()));
            canonical.then(|| digits.parse().ok()).flatten()
        };
        let version = Self {
            major: part()?,
            minor: part()?,
            patch: part()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// This program's version.
    pub(crate) fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION"))
            .expect("Cargo.toml holds a MAJOR.MINOR.PATCH version")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The manifest's name for this build's installer, when the Release workflow
/// publishes one for it: `windows` (x64 setup), `macos` (Apple Silicon disk
/// image) or `linux` (x64 AppImage).
pub(crate) fn platform() -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux")
    } else {
        None
    }
}

/// The installer's file name ending for each platform.
fn installer_suffix(platform: &str) -> &'static str {
    match platform {
        "windows" => "-windows-x64-setup.exe",
        "macos" => "-macos-arm64.dmg",
        _ => "-linux-x64.AppImage",
    }
}

#[derive(Deserialize)]
struct Pointer {
    version: String,
    tag: String,
}

/// The unverified version and tag `latest.json` names.
pub(crate) fn pointer(latest: &[u8]) -> Result<(Version, String)> {
    let pointer: Pointer =
        serde_json::from_slice(latest).context("latest.json is not a manifest")?;
    let version = Version::parse(&pointer.version).context("latest.json has no valid version")?;
    ensure!(
        pointer.tag == format!("v{version}"),
        "latest.json names tag {} for version {version}",
        pointer.tag
    );
    Ok((version, pointer.tag))
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    tag: String,
    files: BTreeMap<String, ManifestFile>,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestFile {
    key: String,
    name: String,
    sha256: String,
    size: u64,
}

/// One platform's installer in a verified release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Release {
    pub version: Version,
    /// The object key below the download base, `releases/<tag>/<name>`.
    pub key: String,
    /// The installer's file name: only ASCII letters, digits, `.`, `_`, `-`.
    pub name: String,
    pub sha256: [u8; 32],
    pub size: u64,
    /// What changed, in plain words: the release page's change list.
    pub notes: Vec<String>,
}

/// The release `manifest` describes for `platform`, when `signature` (base64)
/// is the Ed25519 signature of exactly these bytes by `public_key`.
pub(crate) fn verified_release(
    manifest: &[u8],
    signature: &[u8],
    public_key: &[u8; 32],
    platform: &str,
) -> Result<Release> {
    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature.trim_ascii())
        .context("manifest signature is not base64")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(manifest, &signature)
        .map_err(|_| anyhow::anyhow!("manifest signature does not verify"))?;

    let manifest: Manifest =
        serde_json::from_slice(manifest).context("signed manifest is not valid")?;
    let version =
        Version::parse(&manifest.version).context("signed manifest has no valid version")?;
    ensure!(
        manifest.tag == format!("v{version}"),
        "signed manifest's tag does not match its version"
    );
    let Some(file) = manifest.files.get(platform) else {
        bail!("release {version} has no {platform} installer");
    };
    let safe_name = !file.name.starts_with('.')
        && file
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    ensure!(
        safe_name && file.name == format!("vera20k-{version}{}", installer_suffix(platform)),
        "release {version} names an unexpected installer {:?}",
        file.name
    );
    ensure!(
        file.key == format!("{}/{}", release_folder(version), file.name),
        "release {version} stores its installer at an unexpected key"
    );
    ensure!(
        (1..=MAX_INSTALLER_BYTES).contains(&file.size),
        "release {version} installer size {} is out of range",
        file.size
    );
    let sha256 = decode_sha256(&file.sha256).context("installer SHA-256 is not 64 hex digits")?;
    Ok(Release {
        version,
        key: file.key.clone(),
        name: file.name.clone(),
        sha256,
        size: file.size,
        notes: manifest.notes,
    })
}

/// Where the publisher stores one version's files below the download base.
pub(crate) fn release_folder(version: Version) -> String {
    format!("releases/v{version}")
}

fn decode_sha256(text: &str) -> Option<[u8; 32]> {
    let digits = text.as_bytes();
    if digits.len() != 64 {
        return None;
    }
    let mut digest = [0; 32];
    for (byte, pair) in digest.iter_mut().zip(digits.chunks_exact(2)) {
        let nibble = |digit: u8| match digit {
            b'0'..=b'9' => Some(digit - b'0'),
            b'a'..=b'f' => Some(digit - b'a' + 10),
            _ => None,
        };
        *byte = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Published by the real `.github/release/publish-r2.sh`, signed with a
    // throwaway key: `python -m tools.update_test fixture src/app/updater/fixtures`.
    fn manifest() -> &'static [u8] {
        crate::test_fixture::bytes("src/app/updater/fixtures/manifest.json")
    }

    fn signature() -> &'static [u8] {
        crate::test_fixture::bytes("src/app/updater/fixtures/manifest.json.sig")
    }

    fn public_key() -> [u8; 32] {
        let key = base64::engine::general_purpose::STANDARD
            .decode(crate::test_fixture::text("src/app/updater/fixtures/update-key.pub").trim())
            .unwrap();
        key.try_into().unwrap()
    }

    #[test]
    fn the_publishers_signed_manifest_names_each_platforms_installer() {
        let version = Version::parse("1.2.3").unwrap();
        assert_eq!(pointer(manifest()).unwrap(), (version, "v1.2.3".into()));
        for (platform, size) in [("windows", 45), ("macos", 39), ("linux", 42)] {
            let release =
                verified_release(manifest(), signature(), &public_key(), platform).unwrap();
            assert_eq!(release.version, version);
            assert_eq!(
                release.name,
                format!("vera20k-1.2.3{}", installer_suffix(platform))
            );
            assert_eq!(release.key, format!("releases/v1.2.3/{}", release.name));
            assert_eq!(release.size, size);
            assert_eq!(release.notes.len(), 6);
            assert_eq!(release.notes[3], "Emphasis and links lose their Markdown.");
        }
    }

    #[test]
    fn a_changed_manifest_or_another_key_is_refused() {
        // The Linux installer's SHA-256 begins 7b115c.
        let at = manifest()
            .windows(6)
            .position(|digits| digits == b"7b115c")
            .unwrap();
        let mut changed = manifest().to_vec();
        changed[at] = b'8';
        assert!(verified_release(&changed, signature(), &public_key(), "linux").is_err());
        let mut other = public_key();
        other[31] ^= 1;
        assert!(verified_release(manifest(), signature(), &other, "linux").is_err());
    }

    #[test]
    fn versions_compare_by_number_and_parse_only_canonically() {
        assert!(Version::parse("0.10.0") > Version::parse("0.9.9"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
        for text in [
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-rc1",
            "v1.2.3",
            "1..3",
            "",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?}");
        }
    }
}
