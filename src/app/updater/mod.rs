//! The desktop updater.
//!
//! A game built by the Release workflow (`VERA20K_COMMIT`) and started from
//! its release package checks `latest.json` on a worker thread. A newer
//! release whose manifest verifies against the update key is offered once
//! the main menu is idle, in the shell's own message boxes: Update, Skip This
//! Version or Later, beside what changed (`notes`). Update downloads it with
//! progress in the box, checks its size and SHA-256, installs it and restarts
//! into it. Nothing is asked during a match, and a check that fails, offline
//! included, stays silent.
//! Publication: `.github/release/downloads.md`.

mod fetch;
mod install;
mod manifest;
pub(crate) mod notes;

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use crate::app::App;
use crate::app::frontend::state::{ShellMessageBox, ShellMessageBoxPurpose};
use crate::app::state::AppState;
use crate::ui::main_menu_dialogs::ShellMessageBoxLabels;
use crate::ui::shell::modal::ModalResult;
use crate::util::time::{Duration, Instant};

use fetch::Progress;
use install::Package;
use manifest::{Release, Version};

/// How often a pending check or download is polled.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Set only for the Release workflow's update test (`tools/update_test.py`):
/// its runners have no retail game data, so no main menu to ask on, and the
/// test build updates as soon as it finds a release.
const UNATTENDED: bool = option_env!("VERA20K_UPDATE_UNATTENDED").is_some();

/// The updater's single owner of its progress through one run of the game.
#[derive(Default)]
pub(crate) struct Updater {
    phase: Phase,
}

#[derive(Default)]
enum Phase {
    /// The first poll decides whether to check.
    #[default]
    NotStarted,
    Checking {
        package: Package,
        result: Receiver<anyhow::Result<Option<Release>>>,
    },
    /// A newer release, waiting for an idle main menu.
    Found { package: Package, release: Release },
    /// Its offer is on screen, or the menu is sliding out to show it.
    Offered { package: Package, release: Release },
    Downloading {
        release: Release,
        progress: Arc<Progress>,
        result: Receiver<anyhow::Result<Command>>,
    },
    /// Nothing more this run: no release, declined, failed or restarting.
    Finished,
}

/// Advance the updater; the deadline keeps the event loop polling while a
/// worker runs.
pub(crate) fn poll(state: &mut AppState) -> Option<Instant> {
    match std::mem::take(&mut state.frontend.updater.phase) {
        Phase::NotStarted => {
            state.frontend.updater.phase = start(state);
            matches!(state.frontend.updater.phase, Phase::Checking { .. })
                .then(|| Instant::now() + POLL_INTERVAL)
        }
        Phase::Checking { package, result } => match result.try_recv() {
            Ok(Ok(Some(release))) => {
                log::info!("Updater: VERA20k {} is available", release.version);
                state.frontend.updater.phase = Phase::Found { package, release };
                poll(state)
            }
            Ok(Ok(None)) => {
                log::info!("Updater: no newer release");
                state.frontend.updater.phase = Phase::Finished;
                None
            }
            Ok(Err(error)) => {
                log::info!("Updater: check failed: {error:#}");
                state.frontend.updater.phase = Phase::Finished;
                None
            }
            Err(TryRecvError::Empty) => {
                state.frontend.updater.phase = Phase::Checking { package, result };
                Some(Instant::now() + POLL_INTERVAL)
            }
            Err(TryRecvError::Disconnected) => {
                state.frontend.updater.phase = Phase::Finished;
                None
            }
        },
        Phase::Found { package, release } if UNATTENDED => {
            download(state, package, release);
            Some(Instant::now() + POLL_INTERVAL)
        }
        Phase::Found { package, release } => {
            if main_menu_idle(state) {
                state.frontend.updater.phase = Phase::Offered { package, release };
                App::leave_shell_dialog(
                    state,
                    crate::app::frontend::shell_transition::ShellExitThen::UpdateOffer,
                );
            } else {
                state.frontend.updater.phase = Phase::Found { package, release };
            }
            None
        }
        Phase::Downloading {
            release,
            progress,
            result,
        } => match result.try_recv() {
            Ok(Ok(mut finish)) => {
                state.frontend.updater.phase = Phase::Finished;
                restart(state, &release, &mut finish);
                None
            }
            // Whatever stopped a download the player cancelled, say nothing.
            Ok(Err(_)) if progress.cancel.load(Ordering::Relaxed) => {
                state.frontend.updater.phase = Phase::Finished;
                None
            }
            Ok(Err(error)) => {
                log::warn!("Updater: updating to {} failed: {error:#}", release.version);
                state.frontend.updater.phase = Phase::Finished;
                show_failure(state, release.version, &error);
                None
            }
            Err(TryRecvError::Empty) => {
                if let Some(message_box) = state.frontend.shell_message_box.as_mut()
                    && matches!(message_box.purpose, ShellMessageBoxPurpose::UpdateProgress)
                {
                    message_box.labels.title = progress_text(
                        release.version,
                        progress.received.load(Ordering::Relaxed),
                        release.size,
                    );
                }
                state.frontend.updater.phase = Phase::Downloading {
                    release,
                    progress,
                    result,
                };
                Some(Instant::now() + POLL_INTERVAL)
            }
            Err(TryRecvError::Disconnected) => {
                state.frontend.updater.phase = Phase::Finished;
                None
            }
        },
        phase => {
            state.frontend.updater.phase = phase;
            notes::poll_scroll(state)
        }
    }
}

/// Start the check when this is a release build running from its package
/// and the player can be asked: not a CnCNet-launched match.
fn start(state: &AppState) -> Phase {
    if option_env!("VERA20K_COMMIT").is_none_or(str::is_empty) || state.frontend.external_launch {
        return Phase::Finished;
    }
    let (Some(platform), Some(package)) = (manifest::platform(), install::package()) else {
        return Phase::Finished;
    };
    let skipped = Version::parse(&state.persistence.options_profile.skipped_update);
    let (sender, result) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("vera-update-check".into())
        .spawn(move || {
            let _ = sender.send(fetch::check(Version::current(), skipped, platform));
        });
    match spawned {
        Ok(_) => Phase::Checking { package, result },
        Err(error) => {
            log::warn!("Updater: cannot start the check: {error}");
            Phase::Finished
        }
    }
}

/// The offer waits for the main menu at rest: shown, not sliding, no dialog,
/// movie or error over it.
fn main_menu_idle(state: &AppState) -> bool {
    let frontend = &state.frontend;
    frontend.screen == crate::ui::game_screen::GameScreen::MainMenu
        && frontend.shell_route == crate::app::shell_route::ShellRoute::MainMenu
        && frontend.startup_splash.is_none()
        && frontend.fullscreen_movie.is_none()
        && frontend.credits_roll.is_none()
        && frontend.main_menu_shell_error.is_none()
        && !state.main_menu_dialog_open()
        && !crate::app::frontend::shell_transition::blocks_shell_input(state)
}

/// The menu has slid out: ask over the empty shell backdrop, as Exit Game
/// does (`App::open_exit_confirm`).
pub(crate) fn open_offer(state: &mut AppState) {
    let Phase::Offered { release, .. } = &state.frontend.updater.phase else {
        return;
    };
    let labels = ShellMessageBoxLabels {
        title: format!(
            "VERA20K {} is available. You have {}.\n\nUpdate now? VERA20K restarts when it is done.",
            release.version,
            Version::current()
        ),
        confirm: "Update".to_owned(),
        third: "Skip Version".to_owned(),
        cancel: "Later".to_owned(),
    };
    let notes = notes::ReleaseNotes::new(&state.renderer.bit_font, &release.notes);
    crate::app::frontend::main_menu_shell_render::clear_ra2ts_movie_session(state);
    crate::app::frontend::shell_transition::invalidate_main_menu_dialog_instance(state);
    App::open_shell_message_box(
        state,
        ShellMessageBox {
            labels,
            purpose: ShellMessageBoxPurpose::UpdateOffer(notes),
        },
    );
}

/// The offer's answer: 0 Update, 2 Skip This Version, 1 (or Esc/Enter) Later.
pub(crate) fn answer_offer(state: &mut AppState, result: ModalResult) {
    let Phase::Offered { package, release } = std::mem::take(&mut state.frontend.updater.phase)
    else {
        return;
    };
    state.frontend.updater.phase = Phase::Finished;
    match result {
        ModalResult::MessageBox(0) => download(state, package, release),
        ModalResult::MessageBox(2) => {
            let profile = &mut state.persistence.options_profile;
            profile.skipped_update = release.version.to_string();
            if let Some(config) = state.platform.game_config.as_ref()
                && let Err(error) = profile.commit_skipped_update(config.paths.settings_dir())
            {
                log::warn!("Updater: cannot save the skipped version: {error}");
            }
        }
        _ => {}
    }
    state.platform.window.request_redraw();
}

fn download(state: &mut AppState, package: Package, release: Release) {
    let progress = Arc::new(Progress::default());
    let (sender, result) = mpsc::channel();
    let worker = {
        let progress = Arc::clone(&progress);
        let release = release.clone();
        move || {
            let finish = install::check_replaceable(&package)
                .and_then(|()| install::download_folder())
                .and_then(|folder| {
                    let _ = std::fs::remove_dir_all(&folder);
                    fetch::download(&release, &folder, &progress)
                })
                .and_then(|installer| {
                    // Past this point the update is committed: Cancel no longer applies.
                    anyhow::ensure!(!progress.cancel.load(Ordering::Relaxed), fetch::Cancelled);
                    install::stage(&package, &installer)
                });
            let _ = sender.send(finish);
        }
    };
    if let Err(error) = std::thread::Builder::new()
        .name("vera-update-download".into())
        .spawn(worker)
    {
        show_failure(state, release.version, &anyhow::Error::from(error));
        return;
    }
    open_progress(state, &release, 0);
    state.frontend.updater.phase = Phase::Downloading {
        release,
        progress,
        result,
    };
}

fn open_progress(state: &mut AppState, release: &Release, received: u64) {
    let cancel = App::csf_label(state, "TXT_CANCEL", "Cancel");
    App::open_shell_message_box(
        state,
        ShellMessageBox {
            labels: ShellMessageBoxLabels {
                title: progress_text(release.version, received, release.size),
                confirm: cancel,
                third: String::new(),
                cancel: String::new(),
            },
            purpose: ShellMessageBoxPurpose::UpdateProgress,
        },
    );
}

/// The progress box's button, Esc or Enter. It stops the download; the
/// worker's result still ends the update, so an install already under way
/// finishes and restarts, and its box stays up until then.
pub(crate) fn cancel_download(state: &mut AppState) {
    let Phase::Downloading {
        release, progress, ..
    } = &state.frontend.updater.phase
    else {
        return;
    };
    let received = progress.received.load(Ordering::Relaxed);
    if received >= release.size {
        let release = release.clone();
        open_progress(state, &release, received);
        return;
    }
    progress.cancel.store(true, Ordering::Relaxed);
    log::info!("Updater: download cancelled");
    state.platform.window.request_redraw();
}

fn progress_text(version: Version, received: u64, size: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if received >= size {
        return format!("Installing VERA20K {version}...");
    }
    format!(
        "Downloading VERA20K {version}...\n\n{:.1} of {:.1} MB ({}%)",
        received as f64 / MIB,
        size as f64 / MIB,
        received * 100 / size.max(1)
    )
}

/// The update is in place (or its installer is ready): start what finishes
/// it, then quit the way Exit Game does.
fn restart(state: &mut AppState, release: &Release, finish: &mut Command) {
    match finish.spawn() {
        Ok(child) => {
            // The macOS and Linux helper starts the new version when its
            // standard input ends, which this process's exit closes.
            std::mem::forget(child);
            log::info!("Updater: restarting into VERA20k {}", release.version);
            App::confirm_quit(state);
        }
        Err(error) => {
            log::warn!("Updater: cannot start {:?}: {error}", finish.get_program());
            show_failure(state, release.version, &anyhow::Error::from(error));
        }
    }
}

fn show_failure(state: &mut AppState, version: Version, error: &anyhow::Error) {
    let reason = error.downcast_ref::<install::Hint>().map_or(
        "Try again later, or download it from vera20k.org.",
        |hint| hint.0,
    );
    if state.frontend.shell_message_box.is_some() {
        App::close_shell_message_box_from_controller(state);
    }
    let ok = App::csf_label(state, "TXT_OK", "OK");
    App::open_shell_message_box(
        state,
        ShellMessageBox {
            labels: ShellMessageBoxLabels {
                title: format!("VERA20K could not update to {version}.\n\n{reason}"),
                confirm: ok,
                third: String::new(),
                cancel: String::new(),
            },
            purpose: ShellMessageBoxPurpose::UpdateFailed,
        },
    );
}
