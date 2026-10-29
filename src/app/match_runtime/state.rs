//! Match owner (F12 `MatchState`): the aggregate for everything scoped to a
//! single running match — the authoritative `SimRuntime`, the app-side input,
//! presentation, audio, and diagnostics owners, plus the scenario identity and
//! pacing facts that accompany them.
//!
//! App-side only: nothing here is serialized, hashed, or read by the
//! deterministic simulation except through `SimRuntime` itself.

use crate::map::basic::BasicSection;

pub(crate) struct MatchState {
    pub(crate) startup: super::startup::MatchStartup,
    /// Live session owner: gamemd's `SessionClass` at `0x00A8B238` (who
    /// holds `GameMode`, participants, local participant, host). Written by
    /// the shell, the launch path and the saved-game restore; read by the app
    /// loop only. Read `GameMode` through [`Self::session_mode`].
    pub(crate) session: super::session::MatchSession,
    /// The in-match connection wait and its dialog 0xEA
    /// (`Wait_For_Players 0x00648710`).
    pub(crate) network_wait: super::network_match::NetworkMatchWait,
    /// Input state for ExecuteDoList's blocking checksum-error message box.
    /// The suspended frame and mismatch remain owned by Simulation.
    pub(crate) network_failure: Option<super::network_failure::NetworkFailureModal>,
    /// VERA-only: a network frame failed before commit (`FrameAdvanceError`).
    /// The world keeps partial writes, so it never advances again; the same
    /// checksum box ends the match instead of resuming a suspended frame, with
    /// a body that calls it an error rather than a desync.
    pub(crate) frame_stopped: bool,
    /// Physical byte transport and packet progress for an installed match.
    /// Session owns the accepted participant roster; `sim_runtime` (`SimHost`)
    /// owns the checksum history.
    pub(crate) live_network: Option<crate::net::live_commands::LiveCommandSession>,
    /// LAN visit transport before the match, then discovery metadata while the
    /// same sockets are owned by live_network. Session owns admitted endpoints.
    pub(crate) lan_service: Option<crate::app::lan_service::LanService>,
    /// The process-lifetime simulation host and its installed runtime, if
    /// any. Reads take `runtime()`; writes name a
    /// [`SimWrite`](super::sim_host::SimWrite) route.
    pub(crate) sim_runtime: super::sim_host::SimHost,
    /// Match input owner (F12): camera, zoom, cursor, keys, hotkeys.
    pub(crate) input: crate::app::input::state::MatchInputState,
    /// Match presentation owner (F12), part 1: per-match atlases + cursor.
    pub(crate) match_presentation: crate::app::presentation::state::MatchPresentationState,
    /// Per-match audio owner (F11): sound event queue + EVA latches; resets
    /// on every match install and on leaving a match for the shell.
    pub(crate) match_audio: crate::app::match_audio::MatchAudioState,
    /// App-owned diagnostic recording (F10) — never inside the simulation, so
    /// no load/install path can silently drop an unflushed segment.
    pub(crate) match_diagnostics: crate::app::match_diagnostics::MatchDiagnosticsState,
    /// Online match network quality record; wall-clock, never simulation input.
    pub(crate) network_quality: super::network_quality::NetworkQualityStats,
    /// Read-only process measurements; never inputs to the simulation.
    #[cfg(feature = "telemetry-dashboard")]
    pub(crate) telemetry_timing: super::telemetry::TickTimingRing,
    /// Whole-match 1 Hz timeline; recorded whether or not a dashboard client
    /// is attached, so a late client can backfill.
    #[cfg(feature = "telemetry-dashboard")]
    pub(crate) telemetry_history: super::telemetry::MatchHistory,
    /// Report-grade 5 s samples; each one renders the same report as the live
    /// view, including the time before a client attached.
    #[cfg(feature = "telemetry-dashboard")]
    pub(crate) report_history: super::telemetry::ReportHistory,
    #[cfg(feature = "telemetry-dashboard")]
    pub(crate) telemetry_tick_rate: super::telemetry::TickRateMeter,
    pub(crate) map_basic: BasicSection,
    /// Resolved Full_Init mission strings; retained independently of the
    /// current CSF and outside deterministic simulation state.
    pub(crate) campaign_names: crate::rules::campaign_loading::CampaignScenarioNames,
    /// Parsed `[Ranking]` values used by the campaign victory page.
    pub(crate) map_ranking: crate::map::ranking::RankingSection,
    /// Exact source whose bytes produced the active parsed map.
    pub(crate) loaded_map_source: Option<crate::map::source::LoadedMapSource>,
    /// Deterministic digest of the parsed source map INI. `None` only for
    /// generated/fallback worlds without an authoritative source-map payload.
    pub(crate) loaded_map_hash: Option<u64>,
    /// App-owned wall-clock House Vox drain. HouseState owns pending flags
    /// and SavourDelay; ScenarioSession retains the admitted result request.
    pub(crate) scenario_outcome:
        Option<crate::app::match_runtime::scenario_exit::ScenarioOutcomeVoiceWait>,
    /// Active running-scenario audio teardown. While present the tactical
    /// frame remains visible but simulation is frozen; its destination is
    /// committed only after the retail fade/voice-wait sequence completes.
    pub(crate) scenario_exit: Option<crate::app::match_runtime::scenario_exit::ScenarioExitCascade>,
    /// `g_bScenarioRestartRequested` `0x008B41C0`: B6 Restart raised it and
    /// the termination router consumes it before the next simulation frame.
    pub(crate) scenario_restart_requested: bool,
    /// Match elapsed wall time for the retail score screen. App-local and never
    /// serialized, hashed, or read by deterministic simulation.
    pub(crate) scenario_elapsed_clock: crate::app::match_runtime::frame_pacer::ScenarioElapsedClock,
    /// Config-sourced input delay — copied to each new Simulation instance at game start.
    pub(crate) configured_input_delay_ticks: u64,
    /// Seeded empty-map sandbox keeps full map visibility while still locking control.
    pub(crate) sandbox_full_visibility: bool,
    /// The debug pause (`J` hotkey): the simulation is frozen
    /// without an in-scenario modal. Every modal transition ends it.
    pub(crate) debug_pause: bool,
    /// Debug trainer one-shot: the next tactical left click drops the local
    /// house's MCV on that cell instead of selecting or ordering. Cleared by the
    /// click, by leaving the match, and never serialized.
    pub(crate) trainer_mcv_armed: bool,
    /// Effective simulation ticks per second — controls game speed.
    /// Default follows retail/YR skirmish stored game speed 1.
    pub(crate) sim_speed_tps: u32,
>>>>>>> f152fb851 (offline trainer panel, art bound clamp, and terrain diagnostics)
