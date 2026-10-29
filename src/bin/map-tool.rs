//! Developer map browser and launcher for VERA20K.
//!
//! All parsing is the game's own: `MapFile` for map headers, `map::preview` for
//! `[PreviewPack]` thumbnails, `MISSIONMD.INI` plus the CSF table for a campaign
//! mission's title and briefing, and `MixArchive` for retail map packs. No retail
//! data ships with this tool.
//!
//! ```sh
//! map-tool list <DIR>... [--ra2-dir DIR]
//! map-tool info <MAP> [--ra2-dir DIR]
//! map-tool preview <MAP> --out <PNG>
//! map-tool mix <MIX>... [--extract <ID> --out <FILE>]
//! map-tool launch <MAP> [--ai N] [--campaign <ID> <SIDE>]
//!                      [--data-dir DIR] [--exe PATH]
//! ```

use std::path::{Path, PathBuf};

use vera20k::assets::asset_manager::{AssetManager, MediaArchiveMode};
use vera20k::assets::csf_file::CsfFile;
use vera20k::assets::mix_archive::MixArchive;
use vera20k::map::map_file::MapFile;
use vera20k::map::waypoints::MULTIPLAYER_START_WAYPOINTS;
use vera20k::rules::ini_parser::IniFile;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = arguments.first() else {
        eprintln!("{}", usage());
        std::process::exit(2);
    };
    let result = match verb.as_str() {
        "list" => list(&arguments[1..]),
        "info" => info(&arguments[1..]),
        "preview" => preview(&arguments[1..]),
        "mix" => mix(&arguments[1..]),
        "extract-named" => extract_named(&arguments[1..]),
        "thumb" => thumb(&arguments[1..]),
        "screenshot" => screenshot(&arguments[1..]),
        "radar-preview" => radar_preview(&arguments[1..]),
        "launch" => launch(&arguments[1..]),
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("unknown verb {other:?}\n\n{}", usage())),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn usage() -> &'static str {
    "map-tool — map browser and launcher for VERA20k

USAGE
  map-tool list <DIR>... [--ra2-dir DIR]
      Grouped TSV: group, name, author, size, theater, preview, starts, path.
      A campaign map with an empty [Basic] Name= takes its title from the
      mission's UIName in MISSIONMD.INI.

  map-tool info <MAP> [--ra2-dir DIR]
      The mission's title and briefing text, resolved through ra2md.csf.

  map-tool preview <MAP> --out <PNG>
      Decode the map's [PreviewPack] thumbnail.

  map-tool radar-preview <MAP> --out <PNG> [--wait SECONDS]
      Real whole-map preview: the engine dumps its radar terrain surface (the
      in-game minimap source) once, then exits. No shroud, no stitching.

  map-tool screenshot <MAP> --out <PNG> [--wait SECONDS]
      Render a real preview by starting the map once, capturing the screen and
      cropping the 4:3 playfield. Slow (one game launch) and cached by callers.

  map-tool thumb <MAP> --out <PNG>
      Schematic preview for maps without [PreviewPack]: the isometric playfield
      outline plus the eight multiplayer start markers.

  map-tool extract-named <MIX> <NAME> --out <FILE>
      Pull one file out of a MIX by its filename: retail archives store entries
      under the CRC-32 of the uppercased name, so this needs no name database.

  map-tool mix <MIX>... [--extract <ID> --out <FILE>]
      Maps packed in retail MIX archives, grouped by archive name.

  map-tool launch <MAP> [--ai N] [--campaign <ID> <SIDE>]
                         [--data-dir DIR] [--exe PATH]
      Write spawn.ini plus the map into the game's data folder and start it with
      -SPAWN. A map named ALL*/SOV*/YUR*/C3Y* launches as that campaign mission
      automatically; anything else becomes a skirmish."
}

// --------------------------------------------------------------------------
// Shared reading

/// Mission title and briefing, resolved from `MISSIONMD.INI` and the CSF table.
struct MissionMeta {
    title: String,
    briefing: String,
}

struct MissionAssets {
    ini: IniFile,
    csf: Option<CsfFile>,
}

impl MissionAssets {
    /// Build from one CSF alone: the original RA2 campaign maps name themselves
    /// and their briefings through `ra2.csf`, with no MISSIONMD involvement.
    fn from_csf_only(csf_path: &Path) -> Option<Self> {
        let ini = IniFile::from_bytes(b"").ok()?;
        let csf = std::fs::read(csf_path)
            .ok()
            .and_then(|bytes| CsfFile::from_bytes(&bytes).ok());
        Some(Self { ini, csf })
    }

    /// Build from explicit INI/CSF files instead of the game archives.
    fn from_files(ini_path: &Path, csf_path: Option<&Path>) -> Option<Self> {
        let ini = IniFile::from_bytes(&std::fs::read(ini_path).ok()?).ok()?;
        let csf = csf_path
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| CsfFile::from_bytes(&bytes).ok());
        Some(Self { ini, csf })
    }

    fn load(ra2_dir: Option<&Path>) -> Option<Self> {
        let (ra2_dir, _) = vera20k::asset_tools::root::resolve_ra2_dir(ra2_dir).ok()?;
        let assets = AssetManager::new(&ra2_dir, MediaArchiveMode::STOCK_DIGITAL).ok()?;
        let mission = assets.get_ref("MISSIONMD.INI")?;
        let ini = IniFile::from_bytes(mission).ok()?;
        let csf = assets
            .get_ref("ra2md.csf")
            .and_then(|bytes| CsfFile::from_bytes(bytes).ok());
        Some(Self { ini, csf })
    }

    /// Read `[Basic] Briefing=` straight out of the map file.
    fn map_briefing_key(map_path: &Path) -> Option<String> {
        let bytes = std::fs::read(map_path).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let mut in_basic = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_basic = trimmed.eq_ignore_ascii_case("[Basic]");
                continue;
            }
            if in_basic {
                if let Some(value) = trimmed.strip_prefix("Briefing=") {
                    let value = value.trim();
                    if !value.is_empty() {
                        return Some(value.to_owned());
                    }
                }
            }
        }
        None
    }

    fn meta(&self, map_path: &Path) -> Option<MissionMeta> {
        let stem = map_path.file_stem()?.to_str()?.to_ascii_uppercase();
        let section = self.ini.section(&format!("{stem}.MAP"))?;
        let ui_key = section.read_string("UIName", "", 260);
        let brief_key = section.read_string("Briefing", "", 260);
        let resolve = |key: &str| {
            self.csf
                .as_ref()
                .map_or_else(|| key.to_owned(), |table| table.text(key).into_owned())
        };
        Some(MissionMeta {
            title: resolve(&ui_key),
            briefing: resolve(&brief_key)
                .replace('\n', " ")
                .replace('\r', "")
                .trim()
                .to_owned(),
        })
    }
}

/// How many of the eight multiplayer start waypoints this map defines.
fn start_points(map: &MapFile) -> usize {
    MULTIPLAYER_START_WAYPOINTS
        .filter(|index| {
            map.waypoints
                .get(index)
                .is_some_and(|waypoint| waypoint.rx != 0 || waypoint.ry != 0)
        })
        .count()
}

/// CSF lookup that tolerates the table's own casing: retail mixes `Name:ALL01`
/// and `Name:Sov01` in the same file.
fn csf_text_ci(table: &CsfFile, key: &str) -> Option<String> {
    if let Some(text) = table.get(key) {
        return Some(text.to_owned());
    }
    table
        .entries()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(key))
        .map(|(_, text)| text.to_owned())
}

fn sanitize(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ").trim().to_owned()
}

fn is_map_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "map" | "mpr" | "yrm"))
}

fn collect_maps(roots: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u32, found: &mut Vec<PathBuf>) {
        if depth > 4 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, depth + 1, found);
            } else if is_map_extension(&path) {
                found.push(path);
            }
        }
    }

    let mut found = Vec::new();
    for root in roots {
        if root.is_file() {
            found.push(root.clone());
        } else {
            walk(root, 0, &mut found);
        }
    }
    // Overlapping roots (a folder plus its parent) would otherwise list the same
    // map twice; the canonical path is the identity.
    found.sort();
    found.dedup_by(|a, b| {
        a.canonicalize().ok() == b.canonicalize().ok() && a == b
    });
    found
}

fn group_of(path: &Path) -> String {
    path.parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("maps")
        .to_owned()
}

// --------------------------------------------------------------------------
// Verbs

fn list(arguments: &[String]) -> Result<(), String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut ra2_dir: Option<PathBuf> = None;
    let mut mission_ini: Option<PathBuf> = None;
    let mut csf: Option<PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--ra2-dir" => {
                index += 1;
                ra2_dir = arguments.get(index).map(PathBuf::from);
            }
            "--mission-ini" => {
                index += 1;
                mission_ini = arguments.get(index).map(PathBuf::from);
            }
            "--csf" => {
                index += 1;
                csf = arguments.get(index).map(PathBuf::from);
            }
            value => roots.push(PathBuf::from(value)),
        }
        index += 1;
    }
    if roots.is_empty() {
        return Err(format!("list needs at least one directory\n\n{}", usage()));
    }
    let mission = match (&mission_ini, &csf) {
        (Some(ini), table) => MissionAssets::from_files(ini, table.as_deref()),
        (None, Some(table)) => MissionAssets::from_csf_only(table),
        (None, None) => MissionAssets::load(ra2_dir.as_deref()),
    };

    println!("group\tname\tauthor\tsize\ttheater\tpreview\tstarts\tpath\tmission");
    for path in collect_maps(&roots) {
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skipped unreadable {}", path.display());
            continue;
        };
        let Ok(map) = MapFile::from_bytes(&bytes) else {
            eprintln!("skipped {} (not a map)", path.display());
            continue;
        };
        // A campaign map's own [Basic] Name= is a short code ("A6", "SR2"); the
        // player-facing title is the mission's UIName, so that wins when present.
        let from_mission = mission
            .as_ref()
            .and_then(|assets| assets.meta(&path))
            .or_else(|| {
                // Original RA2 campaign maps carry their own CSF key, and their
                // title lives under `Name:<id>` in ra2.csf.
                let key = MissionAssets::map_briefing_key(&path)?;
                let table = mission.as_ref().and_then(|assets| assets.csf.as_ref());
                let id = key.split(':').nth(1).unwrap_or(&key).to_owned();
                let title_key = format!("Name:{id}");
                let title = table
                    .and_then(|table| csf_text_ci(table, &title_key))
                    .filter(|text| !text.starts_with("MISSING"))
                    .unwrap_or_else(|| id.clone());
                let briefing = table
                    .and_then(|table| csf_text_ci(table, &key))
                    .unwrap_or_else(|| key.clone());
                Some(MissionMeta { title, briefing })
            });
        let name = from_mission
            .as_ref()
            .map(|meta| meta.title.clone())
            .filter(|title| !title.trim().is_empty())
            .or_else(|| {
                map.basic
                    .name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
            })
            .unwrap_or_default();
        // A mission section can leave `UIName=` empty (the co-op chapters do);
        // the CSF lookup then yields a MISSING marker. Fall back to the map's
        // own name, then to the file stem, so the list is never unreadable.
        let name = if name.trim().is_empty() || name.to_ascii_uppercase().starts_with("MISSING") {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_ascii_uppercase())
                .unwrap_or_default()
        } else {
            name
        };
        let mission_id = MissionAssets::map_briefing_key(&path)
            .map(|key| key.rsplit(':').next().unwrap_or(&key).to_ascii_uppercase())
            .unwrap_or_default();
        println!(
            "{}\t{}\t{}\t{}x{}\t{}\t{}\t{}\t{}\t{}",
            sanitize(&group_of(&path)),
            sanitize(&name),
            sanitize(map.basic.author.as_deref().unwrap_or("")),
            map.header.width,
            map.header.height,
            map.header.theater,
            if map.preview.has_packed_preview { "yes" } else { "no" },
            start_points(&map),
            path.display(),
            mission_id
        );
    }
    Ok(())
}

fn info(arguments: &[String]) -> Result<(), String> {
    let mut mission_ini: Option<PathBuf> = None;
    let mut csf: Option<PathBuf> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--mission-ini" => {
                index += 1;
                mission_ini = arguments.get(index).map(PathBuf::from);
            }
            "--csf" => {
                index += 1;
                csf = arguments.get(index).map(PathBuf::from);
            }
            value => rest.push(value.to_owned()),
        }
        index += 1;
    }
    let (map_path, ra2_dir) = single_map_and_dir(&rest, "info")?;
    let mission = match (&mission_ini, &csf) {
        (Some(ini), table) => MissionAssets::from_files(ini, table.as_deref()),
        (None, Some(table)) => MissionAssets::from_csf_only(table),
        (None, None) => MissionAssets::load(ra2_dir.as_deref()),
    }
    .ok_or_else(|| "could not load the mission tables (pass --csf or --ra2-dir)".to_owned())?;
    let meta = mission.meta(&map_path).or_else(|| {
        let key = MissionAssets::map_briefing_key(&map_path)?;
        let id = key.rsplit(':').next().unwrap_or(&key).to_owned();
        let title_key = format!("Name:{id}");
        let title = mission
            .csf
            .as_ref()
            .and_then(|table| csf_text_ci(table, &title_key))
            .filter(|text| !text.starts_with("MISSING"))
            .unwrap_or_else(|| id.clone());
        let briefing = mission
            .csf
            .as_ref()
            .and_then(|table| csf_text_ci(table, &key))
            .unwrap_or(key);
        Some(MissionMeta { title, briefing })
    }).ok_or_else(|| {
        format!(
            "{} has neither a MISSIONMD section nor a [Basic] Briefing key",
            map_path.display()
        )
    })?;
    println!("title\t{}", sanitize(&meta.title));
    println!("briefing\t{}", meta.briefing);
    Ok(())
}

fn preview(arguments: &[String]) -> Result<(), String> {
    let mut map_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--out" => {
                index += 1;
                out_path = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--out needs a path".to_owned())?,
                ));
            }
            value => map_path = Some(PathBuf::from(value)),
        }
        index += 1;
    }
    let map_path = map_path.ok_or_else(|| "preview needs a map path".to_owned())?;
    let out_path = out_path.ok_or_else(|| "preview needs --out <png>".to_owned())?;
    let bytes = std::fs::read(&map_path).map_err(|error| format!("read: {error}"))?;
    let map = MapFile::from_bytes(&bytes).map_err(|error| format!("parse: {error}"))?;
    let Some(decoded) = map.preview.decoded else {
        return Err(format!(
            "{} carries no decodable [PreviewPack]",
            map_path.display()
        ));
    };
    let image = image::RgbaImage::from_raw(decoded.width, decoded.height, decoded.rgba)
        .ok_or_else(|| "preview pixel buffer did not match its size".to_owned())?;
    image
        .save(&out_path)
        .map_err(|error| format!("write {}: {error}", out_path.display()))?;
    println!(
        "{}x{} -> {}",
        decoded.width,
        decoded.height,
        out_path.display()
    );
    Ok(())
}

fn mix(arguments: &[String]) -> Result<(), String> {
    let mut archives: Vec<PathBuf> = Vec::new();
    let mut extract: Option<i32> = None;
    let mut out: Option<PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--extract" => {
                index += 1;
                extract = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--extract needs an entry id".to_owned())?
                        .parse()
                        .map_err(|_| "--extract id must be an i32".to_owned())?,
                );
            }
            "--out" => {
                index += 1;
                out = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--out needs a path".to_owned())?,
                ));
            }
            value => archives.push(PathBuf::from(value)),
        }
        index += 1;
    }
    if archives.is_empty() {
        return Err("mix needs at least one archive path".to_owned());
    }

    println!("group\tname\tauthor\tsize\ttheater\tpreview\tstarts\tid\tarchive");
    for archive_path in archives {
        let archive = MixArchive::load(&archive_path)
            .map_err(|error| format!("open {}: {error}", archive_path.display()))?;
        let group = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mix")
            .to_owned();
        for entry in archive.entries() {
            let Some(bytes) = archive.get_by_id(entry.id) else {
                continue;
            };
            if extract == Some(entry.id) {
                let out = out
                    .as_ref()
                    .ok_or_else(|| "--extract needs --out <file>".to_owned())?;
                std::fs::write(out, bytes)
                    .map_err(|error| format!("write {}: {error}", out.display()))?;
                println!("wrote {} bytes -> {}", bytes.len(), out.display());
            }
            let Ok(map) = MapFile::from_bytes(bytes) else {
                continue;
            };
            println!(
                "{}\t{}\t{}\t{}x{}\t{}\t{}\t{}\t{}\t{}",
                sanitize(&group),
                sanitize(map.basic.name.as_deref().unwrap_or("")),
                sanitize(map.basic.author.as_deref().unwrap_or("")),
                map.header.width,
                map.header.height,
                map.header.theater,
                if map.preview.has_packed_preview { "yes" } else { "no" },
                start_points(&map),
                entry.id,
                archive_path.display()
            );
        }
    }
    Ok(())
}

/// The campaign a mission filename belongs to, or `None` for a skirmish map.
///
/// Retail names each mission after its campaign (`ALL01UMD.MAP`, `SOV03UMD.MAP`,
/// `C3Y01MD.MAP`), so the map file itself decides the side: no picker needed.
fn campaign_for(map_path: &Path) -> Option<(i32, i32)> {
    // A mission's identity is the map's own `[Basic] Briefing=` key first: the
    // original RA2 chapters are packed under hash names, so the file name says
    // nothing and a name-only test would launch them as skirmish (which ends on
    // the first tick, because their scripts expect a mission).
    if let Some(key) = MissionAssets::map_briefing_key(map_path) {
        let id = key.rsplit(':').next().unwrap_or(&key).to_ascii_uppercase();
        if let Some((side, _)) = mission_side(&id) {
            return Some((side, side));
        }
    }
    let stem = map_path.file_stem()?.to_str()?.to_ascii_uppercase();
    mission_side(&stem)
}

/// Side/campaign index for a mission id: `ALL…` is Allied, `SOV…` Soviet and
/// `C3Y…`/`YUR…` Yuri. Training (`TRN…`) plays as Allied.
fn mission_side(id: &str) -> Option<(i32, i32)> {
    if id.starts_with("ALL") || id.starts_with("TRN") {
        Some((0, 0))
    } else if id.starts_with("SOV") {
        Some((1, 1))
    } else if id.starts_with("C3Y") || id.starts_with("YUR") {
        Some((2, 2))
    } else {
        None
    }
}

fn launch(arguments: &[String]) -> Result<(), String> {
    let mut map_path: Option<PathBuf> = None;
    let mut data_dir = PathBuf::from(
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/VERA20k"))
            .ok_or_else(|| "HOME is not set".to_owned())?,
    );
    let mut exe = PathBuf::from("/Applications/VERA20K-trainer.app/Contents/MacOS/VERA20K");
    let mut ai = 1u32;
    let mut campaign: Option<(i32, i32)> = None;
    let mut fullscreen = false;
    let mut ra2_dir: Option<PathBuf> = None;
    let mut stage_name: Option<String> = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--data-dir" => {
                index += 1;
                data_dir = PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--data-dir needs a path".to_owned())?,
                );
            }
            "--exe" => {
                index += 1;
                exe = PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--exe needs a path".to_owned())?,
                );
            }
            "--fullscreen" => fullscreen = true,
            "--stage-name" => {
                index += 1;
                stage_name = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--stage-name needs a name".to_owned())?
                        .to_owned(),
                );
            }
            "--ra2-dir" => {
                index += 1;
                ra2_dir = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--ra2-dir needs a path".to_owned())?,
                ));
            }
            "--ai" => {
                index += 1;
                ai = arguments
                    .get(index)
                    .ok_or_else(|| "--ai needs a count".to_owned())?
                    .parse()
                    .map_err(|_| "--ai must be a number".to_owned())?;
            }
            "--campaign" => {
                index += 1;
                let id: i32 = arguments
                    .get(index)
                    .ok_or_else(|| "--campaign needs an id".to_owned())?
                    .parse()
                    .map_err(|_| "--campaign id must be a number".to_owned())?;
                index += 1;
                let side: i32 = arguments
                    .get(index)
                    .ok_or_else(|| "--campaign needs a side".to_owned())?
                    .parse()
                    .map_err(|_| "--campaign side must be a number".to_owned())?;
                campaign = Some((id, side));
            }
            value => map_path = Some(PathBuf::from(value)),
        }
        index += 1;
    }
    let map_path = map_path.ok_or_else(|| "launch needs a map path".to_owned())?;
    ai = ai.clamp(0, 7);
    std::fs::create_dir_all(&data_dir)
        .map_err(|error| format!("create {}: {error}", data_dir.display()))?;

    // Each launch gets its own directory: `spawn.ini` is read from the working
    // directory and a campaign resolves its map beside it, so a private directory
    // keeps every map's loose file from shadowing another map of the same name
    // (a campaign pack happily reuses `all01umd.map`).
    let file_name = map_path
        .file_name()
        .ok_or_else(|| "map path has no file name".to_owned())?;
    let staged_name = stage_name
        .clone()
        .unwrap_or_else(|| file_name.to_string_lossy().into_owned());
    let slug = format!(
        "{:08x}",
        vera20k::assets::mix_hash::crc_engine(file_name.to_string_lossy().as_bytes())
    );
    let run_dir = data_dir.join("runs").join(&slug);
    std::fs::create_dir_all(&run_dir)
        .map_err(|error| format!("create {}: {error}", run_dir.display()))?;
    let staged = run_dir.join(&staged_name);
    let same_file = map_path
        .canonicalize()
        .ok()
        .zip(staged.canonicalize().ok())
        .is_some_and(|(source, target)| source == target);
    if !same_file {
        std::fs::copy(&map_path, &staged).map_err(|error| format!("stage map: {error}"))?;
    }

    let campaign = campaign.or_else(|| campaign_for(&map_path));
    let text = if let Some((id, side)) = campaign {
        // Mission maps carry their own triggers, so they must start as missions;
        // a skirmish launch ends them on the first tick.
        format!(
            "[Settings]\nName=Player\nScenario={staged_name}\nPlayerCount=1\n\
             IsSinglePlayer=1\nCampaignID={id}\nSide={side}\n\
             DifficultyModeHuman=1\nDifficultyModeComputer=1\nGameSpeed=1\n"
        )
    } else {
        let mut settings = format!(
            "[Settings]\nName=Player\nScenario={staged_name}\nPlayerCount=1\nSide=0\nColor=2\n\
             AIPlayers={ai}\nGameSpeed=1\nSpecialHouseIsAlly=No\nCredits=5000\n\
             Superweapons=No\nAlliesAllowed=Yes\n[HouseHandicaps]\n"
        );
        let mut countries = String::from("[HouseCountries]\n");
        let mut colors = String::from("[HouseColors]\n");
        const AI_COLORS: [u32; 7] = [1, 3, 4, 5, 6, 0, 7];
        for (slot_index, slot) in (2..=(ai + 1)).enumerate() {
            settings.push_str(&format!("Multi{slot}=1\n"));
            countries.push_str(&format!("Multi{slot}={}\n", 8 + (slot % 2)));
            colors.push_str(&format!("Multi{slot}={}\n", AI_COLORS[slot_index]));
        }
        format!("{settings}{countries}{colors}")
    };

    // Fullscreen stays a game-wide preference; the asset root is per process.
    set_fullscreen(&data_dir, fullscreen)?;
    std::fs::write(run_dir.join("spawn.ini"), text)
        .map_err(|error| format!("write spawn.ini: {error}"))?;

    println!(
        "{} -> {} ({})",
        map_path.display(),
        staged.display(),
        if campaign.is_some() {
            "campaign"
        } else {
            "skirmish"
        }
    );

    // Spawn as a child with the run directory as cwd: `-SPAWN` reads
    // `spawn.ini` from there and `RA2_DIR` pins the asset root for this process
    // only, so the shared config is never rewritten.
    // One game at a time: a stale instance keeps running against the *previous*
    // asset root and map, which makes every symptom look like a text or save bug.
    let _ = std::process::Command::new("pkill")
        .args(["-f", "VERA20K-trainer.app/Contents/MacOS/VERA20K"])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(800));

    let mut command = std::process::Command::new(&exe);
    command
        .arg("-SPAWN")
        .current_dir(&run_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.env("RA2_SPAWN_INI", run_dir.join("spawn.ini"));
    if let Some(dir) = &ra2_dir {
        command.env("RA2_DIR", dir);
    }
    command
        .spawn()
        .map_err(|error| format!("start {}: {error}", exe.display()))?;
    Ok(())
}

fn single_map_and_dir(
    arguments: &[String],
    verb: &str,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    let mut map_path: Option<PathBuf> = None;
    let mut ra2_dir: Option<PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--ra2-dir" => {
                index += 1;
                ra2_dir = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--ra2-dir needs a path".to_owned())?,
                ));
            }
            value => map_path = Some(PathBuf::from(value)),
        }
        index += 1;
    }
    let map_path = map_path.ok_or_else(|| format!("{verb} needs a map path"))?;
    Ok((map_path, ra2_dir))
}

/// Force the persisted `[VERA] FullScreen` preference before the game starts.
///
/// The engine owns that key in `VERA.INI` (Options page writes it too); the
/// launcher only flips the one line rather than keeping a second settings model.
fn set_fullscreen(data_dir: &Path, want: bool) -> Result<(), String> {
    let path = data_dir.join("VERA.INI");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let value = if want { "yes" } else { "no" };
    if let Some(section_start) = text.find("[VERA]") {
        let after = &text[section_start..];
        let section_end = after[1..]
            .find("\n[")
            .map_or(text.len(), |offset| section_start + 1 + offset + 1);
        let mut section = text[section_start..section_end].to_owned();
        if section.lines().any(|line| {
            line.trim_start().to_ascii_lowercase().starts_with("fullscreen=")
        }) {
            let mut replaced = String::new();
            for line in section.split_inclusive('\n') {
                if line
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("fullscreen=")
                {
                    replaced.push_str(&format!("FullScreen={value}\n"));
                } else {
                    replaced.push_str(line);
                }
            }
            section = replaced;
        } else {
            if !section.ends_with('\n') {
                section.push('\n');
            }
            section.push_str(&format!("FullScreen={value}\n"));
        }
        text.replace_range(section_start..section_end, &section);
    } else {
        text.push_str(&format!("\n[VERA]\nFullScreen={value}\n"));
    }
    std::fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

/// Point the engine at another asset root (a mod profile) by rewriting the one
/// `ra2_dir` line in `config.toml`. A profile is a directory of symlinks to the
/// base install with the mod's own files overriding them by name, so the base
/// game is never modified.
/// Current `ra2_dir` from the engine's config, so a one-off launch can restore it.
fn read_game_dir(data_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(data_dir.join("config.toml")).ok()?;
    text.lines()
        .find(|line| line.trim_start().starts_with("ra2_dir"))
        .and_then(|line| line.split('"').nth(1))
        .map(str::to_owned)
}

fn set_game_dir(data_dir: &Path, game_dir: &Path) -> Result<(), String> {
    let path = data_dir.join("config.toml");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let value = game_dir.display().to_string();
    if text.lines().any(|line| line.trim_start().starts_with("ra2_dir")) {
        let mut out = String::new();
        for line in text.split_inclusive('\n') {
            if line.trim_start().starts_with("ra2_dir") {
                out.push_str(&format!("ra2_dir = \"{value}\"\n"));
            } else {
                out.push_str(line);
            }
        }
        text = out;
    } else if let Some(paths) = text.find("[paths]") {
        let insert = text[paths..].find('\n').map_or(text.len(), |offset| paths + offset + 1);
        text.insert_str(insert, &format!("ra2_dir = \"{value}\"\n"));
    } else {
        text.push_str(&format!("\n[paths]\nra2_dir = \"{value}\"\n"));
    }
    std::fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

/// Extract a MIX entry by filename.
///
/// Retail MIX files store entries under `mix_hash(name)` and carry no name
/// table, so a known filename is enough to find (and pull out) its bytes.
fn extract_named(arguments: &[String]) -> Result<(), String> {
    let mut archive_path: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--out" => {
                index += 1;
                out = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--out needs a path".to_owned())?,
                ));
            }
            value if archive_path.is_none() => archive_path = Some(PathBuf::from(value)),
            value => name = Some(value.to_owned()),
        }
        index += 1;
    }
    let archive_path = archive_path.ok_or_else(|| "extract-named needs a MIX path".to_owned())?;
    let name = name.ok_or_else(|| "extract-named needs a file name".to_owned())?;
    let out = out.ok_or_else(|| "extract-named needs --out <file>".to_owned())?;

    let archive = MixArchive::load(&archive_path)
        .map_err(|error| format!("open {}: {error}", archive_path.display()))?;
    let id = vera20k::assets::mix_hash::mix_hash(&name);
    let bytes = archive
        .get_by_id(id)
        .ok_or_else(|| format!("{name} (id {id}) is not in {}", archive_path.display()))?;
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&out, bytes).map_err(|error| format!("write {}: {error}", out.display()))?;
    println!("{} bytes -> {}", bytes.len(), out.display());
    Ok(())
}

/// Draw a schematic preview for a map that carries no `[PreviewPack]`.
///
/// Campaign maps never ship a packed thumbnail, so the browser would otherwise
/// show nothing. This renders the one thing the map does state: the playfield
/// bounds and where the multiplayer start waypoints sit. A real tile render needs
/// the theater TMP set, its palette and the iso projection, which belongs to the
/// renderer, not to this tool.
fn thumb(arguments: &[String]) -> Result<(), String> {
    let mut map_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--out" => {
                index += 1;
                out_path = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--out needs a path".to_owned())?,
                ));
            }
            value => map_path = Some(PathBuf::from(value)),
        }
        index += 1;
    }
    let map_path = map_path.ok_or_else(|| "thumb needs a map path".to_owned())?;
    let out_path = out_path.ok_or_else(|| "thumb needs --out <png>".to_owned())?;
    let bytes = std::fs::read(&map_path).map_err(|error| format!("read: {error}"))?;
    let map = MapFile::from_bytes(&bytes).map_err(|error| format!("parse: {error}"))?;

    let width = map.header.width.max(1) as u32;
    let height = map.header.height.max(1) as u32;
    // One pixel per map cell, scaled up so the picture is legible at panel size.
    let cell = (512 / width.max(height)).max(1);
    let image_w = width * cell;
    let image_h = height * cell;
    let mut image = image::RgbaImage::from_pixel(image_w, image_h, image::Rgba([18, 22, 26, 255]));

    // Playfield: an isometric diamond inside the cell rectangle, brighter than
    // the surround so the shape reads even at 1 px per cell.
    let cx = image_w as f32 / 2.0;
    let cy = image_h as f32 / 2.0;
    for y in 0..image_h {
        for x in 0..image_w {
            let dx = (x as f32 - cx).abs() / cx.max(1.0);
            let dy = (y as f32 - cy).abs() / cy.max(1.0);
            if dx + dy <= 1.0 {
                image.put_pixel(x, y, image::Rgba([34, 44, 36, 255]));
            }
        }
    }

    // Start markers, in mission order.
    let mut starts: Vec<(u32, u16, u16)> = MULTIPLAYER_START_WAYPOINTS
        .filter_map(|index| {
            let waypoint = map.waypoints.get(&index)?;
            (waypoint.rx != 0 || waypoint.ry != 0).then_some((index, waypoint.rx, waypoint.ry))
        })
        .collect();
    starts.sort_by_key(|(index, _, _)| *index);
    let start_count = starts.len();
    for (index, rx, ry) in starts {
        // Map cells to the diamond: x scale by the half width, y by the half height.
        let px = (cx + (f32::from(rx) / width as f32 - 0.5) * image_w as f32).clamp(0.0, image_w as f32 - 1.0) as u32;
        let py = (cy + (f32::from(ry) / height as f32 - 0.5) * image_h as f32).clamp(0.0, image_h as f32 - 1.0) as u32;
        let radius = (cell * 2).max(3);
        for dy in 0..=radius * 2 {
            for dx in 0..=radius * 2 {
                let x = px.saturating_add(dx).saturating_sub(radius);
                let y = py.saturating_add(dy).saturating_sub(radius);
                if x < image_w && y < image_h && dx * dx + dy * dy <= radius * radius {
                    let tint = if index % 2 == 0 { [240, 200, 90, 255] } else { [120, 200, 240, 255] };
                    image.put_pixel(x, y, image::Rgba(tint));
                }
            }
        }
    }

    image
        .save(&out_path)
        .map_err(|error| format!("write {}: {error}", out_path.display()))?;
    println!("{}x{} cells, {} starts -> {}", width, height, start_count, out_path.display());
    Ok(())
}

/// Capture a real preview: start the map, screenshot it, crop the playfield.
///
/// Campaign maps carry no `[PreviewPack]`, so the only way to show what a map
/// looks like is to let the engine draw it once. The capture runs full screen so
/// the screen and the playfield share one rectangle; the game is stopped right
/// after the shot.
fn screenshot(arguments: &[String]) -> Result<(), String> {
    let mut map_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut wait = 18u64;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--out" => {
                index += 1;
                out_path = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--out needs a path".to_owned())?,
                ));
            }
            "--wait" => {
                index += 1;
                wait = arguments
                    .get(index)
                    .ok_or_else(|| "--wait needs seconds".to_owned())?
                    .parse()
                    .map_err(|_| "--wait must be a number".to_owned())?;
            }
            value => map_path = Some(PathBuf::from(value)),
        }
        index += 1;
    }
    let map_path = map_path.ok_or_else(|| "screenshot needs a map path".to_owned())?;
    let out_path = out_path.ok_or_else(|| "screenshot needs --out <png>".to_owned())?;

    let data_dir = PathBuf::from(
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/VERA20k"))
            .ok_or_else(|| "HOME is not set".to_owned())?,
    );
    let exe = PathBuf::from("/Applications/VERA20K-trainer.app/Contents/MacOS/VERA20K");
    std::fs::create_dir_all(&data_dir).map_err(|error| format!("create data dir: {error}"))?;

    let file_name = map_path
        .file_name()
        .ok_or_else(|| "map path has no file name".to_owned())?;
    let staged = data_dir.join(file_name);
    if map_path.canonicalize().ok() != staged.canonicalize().ok() {
        std::fs::copy(&map_path, &staged).map_err(|error| format!("stage map: {error}"))?;
    }
    set_fullscreen(&data_dir, true)?;

    let campaign = campaign_for(&map_path);
    let text = if let Some((id, side)) = campaign {
        format!(
            "[Settings]\nName=Player\nScenario={}\nPlayerCount=1\nIsSinglePlayer=1\n\
             CampaignID={id}\nSide={side}\nDifficultyModeHuman=1\nDifficultyModeComputer=1\n\
             GameSpeed=1\n",
            file_name.to_string_lossy()
        )
    } else {
        format!(
            "[Settings]\nName=Player\nScenario={}\nPlayerCount=1\nSide=0\nColor=2\n\
             AIPlayers=0\nGameSpeed=1\nSpecialHouseIsAlly=No\nCredits=5000\n",
            file_name.to_string_lossy()
        )
    };
    std::fs::write(data_dir.join("spawn.ini"), text)
        .map_err(|error| format!("write spawn.ini: {error}"))?;

    let bundle = exe
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf);
    let mut child = match &bundle {
        Some(bundle) => std::process::Command::new("open")
            .args(["-a", &bundle.display().to_string(), "--args", "-SPAWN"])
            .spawn()
            .map_err(|error| format!("open {}: {error}", bundle.display()))?,
        None => std::process::Command::new(&exe)
            .arg("-SPAWN")
            .current_dir(&data_dir)
            .spawn()
            .map_err(|error| format!("start {}: {error}", exe.display()))?,
    };
    println!("rendering {} ({}s)…", map_path.display(), wait);
    std::thread::sleep(std::time::Duration::from_secs(wait));

    let shot = std::env::temp_dir().join("vera20k-screenshot.png");
    let _ = std::fs::remove_file(&shot);
    let status = std::process::Command::new("screencapture")
        .args(["-x", &shot.display().to_string()])
        .status()
        .map_err(|error| format!("screencapture: {error}"))?;
    let _ = child.wait();
    let _ = std::process::Command::new("pkill")
        .args(["-f", "VERA20K-trainer.app/Contents/MacOS/VERA20K"])
        .status();
    if !status.success() {
        return Err("screencapture failed".to_owned());
    }
    set_fullscreen(&data_dir, false)?;

    let screen = image::open(&shot).map_err(|error| format!("open screenshot: {error}"))?;
    let (screen_w, screen_h) = (screen.width(), screen.height());
    // The game runs 4:3 (OriginalShape) inside a full screen frame: keep the
    // centred 4:3 window and drop the side/top bars.
    let target_ratio = 4.0 / 3.0;
    let (crop_w, crop_h) = if screen_w as f32 / screen_h as f32 > target_ratio {
        (((screen_h as f32 * target_ratio) as u32), screen_h)
    } else {
        (screen_w, ((screen_w as f32 / target_ratio) as u32))
    };
    let x = (screen_w - crop_w) / 2;
    let y = (screen_h - crop_h) / 2;
    let preview = screen.crop_imm(x, y, crop_w, crop_h).resize(640, 480, image::imageops::FilterType::Lanczos3);
    preview.save(&out_path).map_err(|error| format!("write {}: {error}", out_path.display()))?;
    println!("preview -> {}", out_path.display());
    Ok(())
}

/// Render a whole-map preview through the engine's radar terrain surface.
///
/// The engine already builds a CPU-side buffer covering every playfield cell for
/// the in-game minimap; `RA2_DUMP_RADAR` makes it write that buffer once. This
/// launches the map with that variable set, waits for the file, and stops the
/// game. No reveal cheats and no stitching are involved.
fn radar_preview(arguments: &[String]) -> Result<(), String> {
    let mut map_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut wait = 45u64;
    let mut ra2_dir: Option<PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--out" => {
                index += 1;
                out_path = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--out needs a path".to_owned())?,
                ));
            }
            "--wait" => {
                index += 1;
                wait = arguments
                    .get(index)
                    .ok_or_else(|| "--wait needs seconds".to_owned())?
                    .parse()
                    .map_err(|_| "--wait must be a number".to_owned())?;
            }
            "--ra2-dir" => {
                index += 1;
                ra2_dir = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--ra2-dir needs a path".to_owned())?,
                ));
            }
            value => map_path = Some(PathBuf::from(value)),
        }
        index += 1;
    }
    let map_path = map_path.ok_or_else(|| "radar-preview needs a map path".to_owned())?;
    let out_path = out_path.ok_or_else(|| "radar-preview needs --out <png>".to_owned())?;

    let data_dir = PathBuf::from(
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/VERA20k"))
            .ok_or_else(|| "HOME is not set".to_owned())?,
    );
    let exe = PathBuf::from("/Applications/VERA20K-trainer.app/Contents/MacOS/VERA20K");
    std::fs::create_dir_all(&data_dir).map_err(|error| format!("create data dir: {error}"))?;

    let file_name = map_path
        .file_name()
        .ok_or_else(|| "map path has no file name".to_owned())?;
    let staged = data_dir.join(file_name);
    if map_path.canonicalize().ok() != staged.canonicalize().ok() {
        std::fs::copy(&map_path, &staged).map_err(|error| format!("stage map: {error}"))?;
    }
    set_fullscreen(&data_dir, false)?;
    let previous_game_dir = read_game_dir(&data_dir);
    if let Some(dir) = &ra2_dir {
        set_game_dir(&data_dir, dir)?;
    }

    let text = if let Some((id, side)) = campaign_for(&map_path) {
        format!(
            "[Settings]\nName=Player\nScenario={}\nPlayerCount=1\nIsSinglePlayer=1\n\
             CampaignID={id}\nSide={side}\nDifficultyModeHuman=1\nDifficultyModeComputer=1\n\
             GameSpeed=1\n",
            file_name.to_string_lossy()
        )
    } else {
        format!(
            "[Settings]\nName=Player\nScenario={}\nPlayerCount=1\nSide=0\nColor=2\n\
             AIPlayers=0\nGameSpeed=1\nSpecialHouseIsAlly=No\nCredits=5000\n",
            file_name.to_string_lossy()
        )
    };
    std::fs::write(data_dir.join("spawn.ini"), text)
        .map_err(|error| format!("write spawn.ini: {error}"))?;

    let _ = std::fs::remove_file(&out_path);
    let mut child = std::process::Command::new(&exe)
        .arg("-SPAWN")
        .current_dir(&data_dir)
        .env("RA2_DUMP_RADAR", &out_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("start {}: {error}", exe.display()))?;

    println!("rendering {} …", map_path.display());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait);
    while std::time::Instant::now() < deadline {
        if out_path.is_file() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let _ = child.kill();
    let _ = child.wait();
    if let Some(previous) = previous_game_dir {
        let _ = set_game_dir(&data_dir, Path::new(&previous));
    }
    if !out_path.is_file() {
        return Err(format!(
            "the game did not dump a radar surface within {wait}s"
        ));
    }
    println!("preview -> {}", out_path.display());
    Ok(())
}
