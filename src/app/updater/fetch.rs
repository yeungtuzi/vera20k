//! The updater's HTTP, on worker threads only: the check reads `latest.json`
//! and, only when it names a newer version, that version's signature; the
//! download streams the installer to disk while hashing it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use base64::Engine;

use super::manifest::{self, Release, Version};

/// The download base: `latest.json` and `releases/` sit below it
/// (`.github/release/downloads.md`). A test build names its own server.
const BASE_URL: &str = match option_env!("VERA20K_UPDATE_URL") {
    Some(url) => url,
    None => "https://downloads.vera20k.org/",
};

/// The base64 Ed25519 public key a manifest's signature must verify with
/// (`.github/release/update-key.pub`). A test build names its own key.
const PUBLIC_KEY: &str = match option_env!("VERA20K_UPDATE_KEY") {
    Some(key) => key,
    None => include_str!("../../../.github/release/update-key.pub"),
};

const USER_AGENT: &str = concat!("VERA20k/", env!("CARGO_PKG_VERSION"), " updater");

/// `latest.json` is under 1 KiB and a signature 88 bytes.
const MAX_POINTER_BYTES: u64 = 64 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 1024;

/// Shared between the download worker and the progress box.
#[derive(Default)]
pub(super) struct Progress {
    pub received: AtomicU64,
    pub cancel: AtomicBool,
}

/// The player cancelled the download.
#[derive(Debug, thiserror::Error)]
#[error("the download was cancelled")]
pub(super) struct Cancelled;

fn public_key() -> Result<[u8; 32]> {
    base64::engine::general_purpose::STANDARD
        .decode(PUBLIC_KEY.trim())
        .ok()
        .and_then(|key| key.try_into().ok())
        .context("the update key is not a base64 Ed25519 public key")
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(BASE_URL.starts_with("https://"))
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // A stalled transfer fails; a slow one may take as long as it needs.
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    let response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("cannot reach {url}"))?;
    ensure!(
        response.status() == 200,
        "{url} answered {}",
        response.status()
    );
    Ok(response)
}

fn get_small(agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    get(agent, url)?
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {url}"))?;
    ensure!(
        bytes.len() as u64 <= limit,
        "{url} is larger than a manifest"
    );
    Ok(bytes)
}

/// The newest release for `platform` when it is newer than `current` and
/// not the version the player skipped, verified against the update key.
pub(super) fn check(
    current: Version,
    skipped: Option<Version>,
    platform: &str,
) -> Result<Option<Release>> {
    let agent = agent();
    let latest = get_small(&agent, &format!("{BASE_URL}latest.json"), MAX_POINTER_BYTES)?;
    let (version, _) = manifest::pointer(&latest)?;
    if skipped == Some(version) {
        log::info!("Updater: VERA20k {version} was skipped");
    }
    if version <= current || skipped == Some(version) {
        return Ok(None);
    }
    let signature = get_small(
        &agent,
        &format!(
            "{BASE_URL}{}/manifest.json.sig",
            manifest::release_folder(version)
        ),
        MAX_SIGNATURE_BYTES,
    )?;
    manifest::verified_release(&latest, &signature, &public_key()?, platform).map(Some)
}

/// Download `release` into `folder` and return the installer's path once its
/// size and SHA-256 match the signed manifest. Nothing is left behind on
/// failure or [`Cancelled`].
pub(super) fn download(release: &Release, folder: &Path, progress: &Progress) -> Result<PathBuf> {
    let url = format!("{BASE_URL}{}", release.key);
    let mut response = get(&agent(), &url)?;
    save(response.body_mut().as_reader(), release, folder, progress)
        .with_context(|| format!("cannot download {url}"))
}

fn save(
    source: impl Read,
    release: &Release,
    folder: &Path,
    progress: &Progress,
) -> Result<PathBuf> {
    std::fs::create_dir_all(folder)
        .with_context(|| format!("cannot create {}", folder.display()))?;
    let target = folder.join(&release.name);
    let partial = folder.join(format!("{}.part", release.name));
    let result = save_to(source, release, &partial, progress)
        .and_then(|()| std::fs::rename(&partial, &target).context("cannot keep the download"));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result.map(|()| target)
}

fn save_to(
    mut source: impl Read,
    release: &Release,
    path: &Path,
    progress: &Progress,
) -> Result<()> {
    let mut file =
        std::fs::File::create(path).with_context(|| format!("cannot write {}", path.display()))?;
    let mut digest = crate::util::sha256::Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut received = 0u64;
    loop {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        received += count as u64;
        ensure!(
            received <= release.size,
            "{} is larger than its manifest says",
            release.name
        );
        digest.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .with_context(|| format!("cannot write {}", path.display()))?;
        progress.received.store(received, Ordering::Relaxed);
    }
    ensure!(received == release.size, "{} ended early", release.name);
    ensure!(
        digest.finalize() == release.sha256,
        "{} does not match its signed SHA-256",
        release.name
    );
    file.sync_all()
        .with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_installer_matching_its_signed_sha256_is_kept() {
        let folder =
            std::env::temp_dir().join(format!("vera20k-update-save-{}", std::process::id()));
        let installer = b"installer bytes";
        let mut digest = crate::util::sha256::Sha256::new();
        digest.update(installer);
        let mut release = Release {
            version: Version::parse("1.2.3").unwrap(),
            key: "releases/v1.2.3/vera20k-1.2.3-macos-arm64.dmg".into(),
            name: "vera20k-1.2.3-macos-arm64.dmg".into(),
            sha256: digest.finalize(),
            size: installer.len() as u64,
            notes: Vec::new(),
        };
        let changed = b"installer bytez";
        let refused = save(&changed[..], &release, &folder, &Progress::default());
        assert!(refused.unwrap_err().to_string().contains("SHA-256"));
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
        let kept = save(&installer[..], &release, &folder, &Progress::default()).unwrap();
        assert_eq!(std::fs::read(&kept).unwrap(), installer);
        release.size += 1;
        assert!(save(&installer[..], &release, &folder, &Progress::default()).is_err());
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
