# Reunion Lite patches

This fork carries four small, platform-neutral patches that the
[Reunion Lite](https://github.com/yeungtuzi/reunion-lite) launcher needs on
Linux/aarch64 (M1000/MTGPU). `main` is upstream `YuriPlanet/vera20k` plus these
commits; nothing is gated behind a target OS.

| Patch | Env / behaviour | Why |
|---|---|---|
| Team AI tolerance | `VERA_TOLERANT_AI=1` | Reunion campaign AIMD files name task-force members the active RuleSet cannot resolve (`UnknownTaskForceMember`). Upstream refuses the whole install and the map fails to load; with the variable set the unknown members stay dropped and the rest of the team AI installs. |
| Performance readout | `VERA_FPS_COUNTER=1` | Starts with the FPS/CPU panel visible instead of pressing Ctrl+Alt+F. |
| Audio period | `VERA_AUDIO_BUFFER=<frames>` | The default ~10 ms SFX period underruns while a mission loads on slow ARM hosts (audible crackle); 2048 removes it. |
| Shutdown summary | always | Logs `Performance summary: adapter=..., frames=..., seconds=..., avg_fps=...` on a clean exit, so a run leaves evidence instead of only the on-screen panel. |

## Build

```sh
cargo build --release          # native host (linux-aarch64, macOS, Windows)
```

## Keep in sync with upstream

```sh
git remote add upstream https://github.com/YuriPlanet/vera20k.git
git fetch upstream main
git rebase upstream/main      # the four patches are small and rebase cleanly
```

## Reunion Lite usage

Point the launcher at `target/release/vera20k` (Settings → engine → VERA20K),
choose `VERA20K graphics backend = auto` and the launcher sets
`VERA_TOLERANT_AI`, `VERA_FPS_COUNTER`/`VERA_AUDIO_BUFFER` and
`VK_ICD_FILENAMES` as needed.
