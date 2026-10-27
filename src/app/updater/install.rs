//! Putting a verified installer in place of the running game, and starting the
//! new version once this process has exited.
//!
//! - Windows: the Inno Setup installer (`.github/release/windows/vera20k.iss`)
//!   runs silently with `/UPDATE=<this process id>` into this install folder.
//!   It waits for this process to exit, keeps config.toml as it is, installs
//!   over the old files and starts the game.
//! - macOS: the new VERA20K.app from the disk image takes the running
//!   bundle's place; the old bundle is deleted once this process has exited.
//! - Linux: the new AppImage replaces the running one's file.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};

/// A reason the player can act on, shown instead of the generic failure.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(super) struct Hint(pub &'static str);

/// The release package this process runs from: what an update replaces.
#[derive(Debug, Clone)]
pub(super) enum Package {
    /// The Inno Setup install folder: vera20k.exe beside its uninstaller.
    Windows(PathBuf),
    /// The app bundle, normally /Applications/VERA20K.app.
    MacApp(PathBuf),
    /// The AppImage file.
    AppImage(PathBuf),
}

pub(super) fn package() -> Option<Package> {
    if cfg!(target_os = "windows") {
        let executable = std::env::current_exe().ok()?;
        let folder = executable.parent()?;
        folder
            .join("unins000.exe")
            .is_file()
            .then(|| Package::Windows(folder.to_path_buf()))
    } else if cfg!(target_os = "macos") {
        crate::app::app_bundle::package_path().map(Package::MacApp)
    } else {
        crate::app::app_bundle::package_path().map(Package::AppImage)
    }
}

/// Where installers download to, replaced by each download: in the player's
/// own VERA20k folder, which no other user can write to. Linux's shared /tmp
/// would let one plant the folder and swap the verified installer.
pub(super) fn download_folder() -> Result<PathBuf> {
    Ok(crate::util::config::default_settings_dir()?.join("update"))
}

/// Whether `package` can be replaced where it is, checked before downloading.
pub(super) fn check_replaceable(package: &Package) -> Result<()> {
    match package {
        Package::Windows(folder) => {
            ensure!(
                writable_folder(folder),
                Hint(
                    "VERA20K cannot replace its files in this folder. Download the new version from vera20k.org."
                )
            );
            Ok(())
        }
        Package::MacApp(bundle) => {
            // macOS runs an app opened where it was downloaded from a
            // randomized read-only copy (App Translocation), or from the disk
            // image itself; neither can be replaced.
            let translocated = bundle
                .components()
                .any(|part| part.as_os_str() == "AppTranslocation");
            ensure!(
                !translocated && bundle.parent().is_some_and(writable_folder),
                Hint("Move VERA20K to your Applications folder and start it from there to update.")
            );
            Ok(())
        }
        Package::AppImage(image) => {
            ensure!(
                image.parent().is_some_and(writable_folder),
                Hint("VERA20K cannot replace its AppImage in this folder.")
            );
            Ok(())
        }
    }
}

fn writable_folder(folder: &Path) -> bool {
    let probe = folder.join(format!(".vera20k-update-{}", std::process::id()));
    let writable = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    writable
}

/// Put the verified `installer` in place where that is part of updating, and
/// return the command that finishes the update once this process exits.
pub(super) fn stage(package: &Package, installer: &Path) -> Result<Command> {
    match package {
        Package::Windows(folder) => Ok(windows_setup(folder, installer)),
        Package::MacApp(bundle) => replace_app_bundle(bundle, installer),
        Package::AppImage(image) => replace_app_image(image, installer),
    }
}

/// The installer's silent update: it shows its own progress window, and its
/// `/LOG` explains a failure.
fn windows_setup(folder: &Path, installer: &Path) -> Command {
    let mut command = Command::new(installer);
    command
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"])
        .arg(format!("/UPDATE={}", std::process::id()))
        .arg(format!("/DIR={}", folder.display()))
        .arg(format!(
            "/LOG={}",
            installer.with_extension("log").display()
        ));
    command
}

fn replace_app_bundle(bundle: &Path, image: &Path) -> Result<Command> {
    let folder = bundle.parent().context("the app bundle has no folder")?;
    // Beside the bundle, so both renames below stay on one volume.
    let staging = folder.join(".VERA20K-update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging)
        .with_context(|| format!("cannot create {}", staging.display()))?;
    let mount = image.with_extension("mount");
    let _ = std::fs::remove_dir_all(&mount);
    std::fs::create_dir_all(&mount)?;
    run(Command::new("/usr/bin/hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&mount)
        .arg(image))?;
    let new_bundle = staging.join("VERA20K.app");
    let copied = run(Command::new("/usr/bin/ditto")
        .arg(mount.join("VERA20K.app"))
        .arg(&new_bundle));
    if let Err(error) = run(Command::new("/usr/bin/hdiutil")
        .args(["detach", "-force"])
        .arg(&mount))
    {
        log::warn!("Updater: {error:#}");
    }
    copied?;
    // The release seals the bundle with an ad hoc signature; an incomplete
    // copy fails this check instead of becoming the installed game.
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(&new_bundle))?;
    let old_bundle = staging.join("old.app");
    std::fs::rename(bundle, &old_bundle)
        .with_context(|| format!("cannot move {}", bundle.display()))?;
    if let Err(error) = std::fs::rename(&new_bundle, bundle) {
        let _ = std::fs::rename(&old_bundle, bundle);
        return Err(error).with_context(|| format!("cannot replace {}", bundle.display()));
    }
    let mut open = Command::new("/usr/bin/open");
    open.arg(bundle);
    Ok(after_exit(&open, Some(&staging)))
}

fn replace_app_image(image: &Path, download: &Path) -> Result<Command> {
    let name = image.file_name().context("the AppImage has no file name")?;
    let staged = image.with_file_name(format!(".{}.update", name.to_string_lossy()));
    std::fs::copy(download, &staged)
        .with_context(|| format!("cannot write {}", staged.display()))?;
    let replaced = make_executable(&staged)
        .and_then(|()| std::fs::File::open(&staged)?.sync_all())
        .and_then(|()| std::fs::rename(&staged, image));
    if replaced.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    replaced.with_context(|| format!("cannot replace {}", image.display()))?;
    // The new image's runtime names its own mount; inherited values would
    // point at this process's.
    let mut start = Command::new(image);
    for variable in ["APPDIR", "APPIMAGE", "ARGV0", "OWD"] {
        start.env_remove(variable);
    }
    Ok(after_exit(&start, None))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}

/// A detached shell that waits for this process to exit, removes `cleanup`,
/// then runs `program` with its arguments and environment changes.
fn after_exit(program: &Command, cleanup: Option<&Path>) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(
            "pid=$1 cleanup=$2; shift 2\n\
             while kill -0 \"$pid\" 2>/dev/null; do sleep 0.2; done\n\
             [ -z \"$cleanup\" ] || rm -rf -- \"$cleanup\"\n\
             exec \"$@\"",
        )
        .arg("vera20k-restart")
        .arg(std::process::id().to_string())
        .arg(cleanup.map_or_else(Default::default, Path::as_os_str))
        .arg(program.get_program())
        .args(program.get_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (variable, value) in program.get_envs() {
        match value {
            Some(value) => command.env(variable, value),
            None => command.env_remove(variable),
        };
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
}

/// Run a helper to completion; its stderr explains a failure.
fn run(command: &mut Command) -> Result<()> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", command.get_program().to_string_lossy()))?;
    ensure!(
        output.status.success(),
        "{} failed: {}",
        command.get_program().to_string_lossy(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}
