//! Steam library enumeration — pure Rust, cross-platform.
//!
//! Locates the Steam install root per-OS, reads `steamapps/libraryfolders.vdf`
//! to discover every library folder, then parses each `appmanifest_*.acf`
//! (Valve KeyValues) into a [`LibraryEntry`]. Runtime junk (Proton, the Steam
//! Linux Runtime, Steamworks redistributables) is filtered out so only real
//! games surface.
//!
//! Everything here is pure (no network, no daemon state), so the parser is
//! unit-tested on every platform against a checked-in `appmanifest` fixture.

use keyvalues_parser::Vdf;
use std::path::{Path, PathBuf};
use tv_shell_protocol::LibraryEntry;

/// `StateFlags` bit indicating a fully-installed app (Steam's
/// `k_EAppStateFullyInstalled`). A manifest can exist for a game that's only
/// partially downloaded; we still report it but flag `installed = false`.
const STATE_FLAG_FULLY_INSTALLED: u64 = 4;

/// Known non-game appids: Proton builds, Steam Linux Runtime, Steamworks Common
/// Redistributables. These ship `appmanifest`s but are tooling, not games.
const RUNTIME_APPIDS: &[u32] = &[
    228980,  // Steamworks Common Redistributables
    1070560, // Steam Linux Runtime 1.0 (scout)
    1391110, // Steam Linux Runtime 2.0 (soldier)
    1493710, // Proton Experimental
    1628350, // Steam Linux Runtime 3.0 (sniper)
    2180100, // Proton Hotfix
    4183110, // (historical Proton/runtime appid)
    4628710, // (historical Proton/runtime appid)
];

/// Name prefixes that mark a manifest as runtime tooling rather than a game.
/// Lower-cased prefix match against the manifest `name`.
const RUNTIME_NAME_PREFIXES: &[&str] = &[
    "proton",
    "steam linux runtime",
    "steamworks common redistributables",
];

/// Return the candidate Steam install roots for the current OS, in priority
/// order. Only existing directories are kept by the caller.
fn steam_roots() -> Vec<PathBuf> {
    let home = home_dir();
    let mut roots = Vec::new();

    #[cfg(target_os = "linux")]
    {
        if let Some(h) = &home {
            roots.push(h.join(".steam/steam"));
            roots.push(h.join(".local/share/Steam"));
            // Flatpak Steam.
            roots.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(h) = &home {
            roots.push(h.join("Library/Application Support/Steam"));
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Registry would be more precise (HKCU\Software\Valve\Steam\SteamPath),
        // but the default install path covers the overwhelming majority and
        // keeps this dependency-free. A `STEAM_PATH` override is honored first.
        if let Ok(p) = std::env::var("STEAM_PATH") {
            roots.push(PathBuf::from(p));
        }
        roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
        roots.push(PathBuf::from(r"C:\Program Files\Steam"));
    }

    // Allow an explicit override on any OS (used by tests and unusual installs).
    if let Some(p) = tv_shell_protocol::brand::env("STEAM_ROOT") {
        roots.insert(0, PathBuf::from(p));
    }

    let _ = &home; // silence unused warning on OSes with no home-based root.
    roots
}

/// Best-effort home directory without pulling in a crate (`$HOME` on
/// Unix, `%USERPROFILE%` on Windows).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Enumerate all installed games across every Steam library folder.
///
/// Returns an empty list (not an error) when Steam isn't installed or no
/// manifests are found — the widget renders "no games" the same as an empty
/// library. Hard I/O errors on individual manifests are skipped, not fatal.
pub fn enumerate() -> Vec<LibraryEntry> {
    let Some(steam_root) = steam_roots().into_iter().find(|r| r.is_dir()) else {
        tracing::debug!("steam: no install root found");
        return Vec::new();
    };

    let mut entries = Vec::new();
    for lib in library_folders(&steam_root) {
        let steamapps = lib.join("steamapps");
        let Ok(read) = std::fs::read_dir(&steamapps) else {
            continue;
        };
        for dirent in read.flatten() {
            let path = dirent.path();
            if !is_appmanifest(&path) {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    if let Some(entry) = parse_appmanifest(&text) {
                        if !is_runtime(&entry) {
                            entries.push(entry);
                        }
                    }
                }
                Err(e) => tracing::debug!("steam: read {} failed: {e}", path.display()),
            }
        }
    }

    // Stable order: name (case-insensitive) so the daemon's name-sorted rail is
    // deterministic even before it re-sorts.
    entries.sort_by_key(|e| e.name.to_lowercase());
    entries.dedup_by_key(|e| e.appid);
    entries
}

/// Is this a `appmanifest_<appid>.acf` file?
fn is_appmanifest(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with("appmanifest_") && name.ends_with(".acf")
}

/// Resolve every library folder from `<steam_root>/steamapps/libraryfolders.vdf`.
/// The Steam root itself is always included (it's library 0). Parse failures
/// degrade to just the root.
fn library_folders(steam_root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![steam_root.to_path_buf()];
    let vdf_path = steam_root.join("steamapps/libraryfolders.vdf");
    if let Ok(text) = std::fs::read_to_string(&vdf_path) {
        for p in parse_library_folders(&text) {
            let p = PathBuf::from(p);
            if !libs.contains(&p) {
                libs.push(p);
            }
        }
    }
    libs
}

/// Parse `libraryfolders.vdf` → the `"path"` of every library entry. Pure
/// function (unit-tested). The file shape is:
/// `"libraryfolders" { "0" { "path" "/a" ... } "1" { "path" "/b" ... } }`.
pub fn parse_library_folders(text: &str) -> Vec<String> {
    let Ok(vdf) = keyvalues_parser::parse(text).map(Vdf::from) else {
        return Vec::new();
    };
    let Some(root) = vdf.value.get_obj() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    // Each numeric-keyed child is a library; pull its "path".
    for values in root.values() {
        for v in values {
            if let Some(obj) = v.get_obj() {
                if let Some(path) = obj
                    .get("path")
                    .and_then(|vs| vs.first())
                    .and_then(|v| v.get_str())
                {
                    paths.push(path.to_string());
                }
            }
        }
    }
    paths
}

/// Parse one `appmanifest_*.acf` into a [`LibraryEntry`]. Returns `None` when
/// the manifest lacks an `appid` (the one field we can't synthesize). Pure
/// function — unit-tested against a fixture.
pub fn parse_appmanifest(text: &str) -> Option<LibraryEntry> {
    // Distinguish a genuinely unparseable (corrupt/truncated) manifest from a
    // valid one that simply lacks fields: log the VDF error so a bad ACF doesn't
    // vanish silently, then skip it.
    let vdf = match keyvalues_parser::parse(text).map(Vdf::from) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("appmanifest vdf parse failed: {e}");
            return None;
        }
    };
    // The root key is "AppState"; its value is the object with our fields.
    let obj = vdf.value.get_obj()?;

    let field = |key: &str| -> Option<String> {
        obj.get(key)
            .and_then(|vs| vs.first())
            .and_then(|v| v.get_str())
            .map(|s| s.to_string())
    };

    let appid = field("appid")?.trim().parse::<u32>().ok()?;
    let name = field("name").unwrap_or_else(|| format!("App {appid}"));
    let last_played = field("LastPlayed")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&t| t > 0);
    let size_on_disk = field("SizeOnDisk").and_then(|s| s.trim().parse::<u64>().ok());
    let state_flags = field("StateFlags")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let installed = state_flags & STATE_FLAG_FULLY_INSTALLED != 0;

    Some(LibraryEntry {
        appid,
        name,
        last_played,
        size_on_disk,
        installed,
    })
}

/// The currently-running Steam appid, or `None` when nothing is running.
///
/// The detection source is OS-specific because Steam exposes the running game
/// differently per platform:
///
/// - **Linux**: scan `/proc/*/cmdline` for Steam's `reaper` launcher process,
///   whose argv is `reaper SteamLaunch AppId=<appid> -- <proton/...> <game.exe>`.
///   Linux Steam does NOT write `RunningAppID` to `registry.vdf` (that's a
///   Windows-only field), so the process scan is the only reliable signal.
/// - **Windows**: read the registry DWORD `HKCU\Software\Valve\Steam\RunningAppID`
///   via a `reg query` shell-out, falling back to parsing the on-disk
///   `registry.vdf` (which Steam also maintains) if the command fails or its
///   output is unparseable — then **cross-check the claim against a live
///   process** ([`appid_has_live_process`]). The registry value is a *claim*, not
///   a liveness signal: Steam does not clear `RunningAppID` when its client
///   crashes, so a crashed Steam leaves a phantom "running" appid behind
///   indefinitely. The cross-check reuses the exact process matcher
///   [`quit_windows`] uses, so status and quit can no longer disagree (a stale
///   id reporting "running" while quit finds nothing to kill was the bug).
/// - **macOS**: not wired yet — returns `None` (out of scope).
///
/// Linux needs no such cross-check: its source *is* a live process (`reaper` in
/// `/proc`), not a persisted value, and a dead pid simply disappears from the
/// scan. See [`running_appid_linux`].
///
/// Returns `None` whenever nothing matches — the "Playing" badge then shows no
/// running game.
pub fn running_appid() -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        running_appid_linux()
    }

    #[cfg(target_os = "windows")]
    {
        running_appid_windows()
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        // macOS (and any other OS): no running-game detection wired yet. Steam
        // on mac keeps a `registry.vdf`, but without a Linux-style `reaper`
        // process or a Windows registry key it's out of scope for now.
        None
    }
}

/// Linux running-game detector: scan `/proc/*/cmdline` for Steam's `reaper`
/// launcher and return its `AppId=<digits>`. `/proc` is read with pure std (no
/// new crates); each `cmdline` is NUL-separated argv. Unreadable entries
/// (permissions, races where the pid exits mid-scan) are skipped.
///
/// **This is already a liveness check** — unlike the Windows registry value, the
/// source here is the live process table, so there is nothing stale to guard
/// against: when the game (and its `reaper`) exit, the `/proc` entry is gone and
/// the scan reports nothing. A lingering zombie doesn't fake it either — the
/// kernel serves an empty `cmdline` for a zombie, so it can't carry the
/// `SteamLaunch`/`AppId=` tokens the matcher requires. It is also the same scan
/// [`quit_linux`] uses (both go through [`steam_appid_from_argv`]), so the two
/// sides agree by construction.
#[cfg(target_os = "linux")]
fn running_appid_linux() -> Option<u32> {
    let proc = std::fs::read_dir("/proc").ok()?;
    for dirent in proc.flatten() {
        // Only numeric (pid) directories carry a cmdline worth reading.
        let name = dirent.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(raw) = std::fs::read(dirent.path().join("cmdline")) else {
            continue;
        };
        // cmdline is NUL-separated; split, drop the trailing empty field, and
        // keep only valid UTF-8 args (Steam's launcher args are plain ASCII).
        let args: Vec<&str> = raw
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect();
        if let Some(appid) = steam_appid_from_argv(&args) {
            return Some(appid);
        }
    }
    None
}

/// Pure argv → appid extractor for the Linux `reaper` launcher. A running Steam
/// game has a process whose args contain the token `SteamLaunch` followed by an
/// `AppId=<digits>` arg, e.g.
/// `reaper SteamLaunch AppId=2215200 -- <proton/...> <game.exe>`.
/// Returns the parsed appid, or `None` when the marker/appid is absent or
/// unparseable. Unit-tested without touching `/proc`.
#[cfg(any(target_os = "linux", test))]
fn steam_appid_from_argv(args: &[&str]) -> Option<u32> {
    if !args.contains(&"SteamLaunch") {
        return None;
    }
    args.iter()
        .find_map(|a| a.strip_prefix("AppId="))
        .and_then(|id| id.parse::<u32>().ok())
}

/// Pure predicate: is this the `reaper` launcher argv for the given `appid`? Reuses
/// [`steam_appid_from_argv`] (so the `SteamLaunch` marker + `AppId=` extraction
/// stay one source of truth) and compares the parsed id. Unit-tested without
/// touching `/proc`.
#[cfg(any(target_os = "linux", test))]
fn argv_matches_appid(args: &[&str], appid: u32) -> bool {
    steam_appid_from_argv(args) == Some(appid)
}

/// Gracefully terminate the running Steam game for `appid` — the host side of
/// `steam-quit`, the equivalent of pressing Steam's Stop button.
///
/// - **Linux**: find the `reaper` launcher pid whose argv matches `SteamLaunch
///   AppId=<appid>` (same `/proc` scan as [`running_appid_linux`]) and send
///   **SIGTERM to its process group** (`kill(-pid, SIGTERM)`) so the whole game
///   process tree shuts down cleanly. Graceful only — never SIGKILL. Returns
///   `Ok(true)` if a matching process was signalled, `Ok(false)` if no such game
///   is running (nothing to do).
/// - **Windows**: resolve the appid's install directory, enumerate running
///   processes, and `taskkill /PID <pid>` (no `/F`, graceful) every process whose
///   executable is unambiguously inside that directory. See [`quit_windows`] for
///   the full safety design.
/// - **Other OSes** (macOS): not wired yet — returns `Ok(false)` (unsupported),
///   mirroring how [`running_appid`] degrades on non-Linux/Windows.
pub fn quit(appid: u32) -> anyhow::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        quit_linux(appid)
    }

    #[cfg(target_os = "windows")]
    {
        quit_windows(appid)
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        // No running-game termination wired off Linux/Windows. Report "nothing
        // signalled" so the caller surfaces a clean not-running/unsupported
        // reply rather than an error.
        let _ = appid;
        Ok(false)
    }
}

/// Linux graceful-quit: locate the `reaper` pid for `appid`, then SIGTERM its
/// process group. Returns `Ok(false)` when no matching game is running.
#[cfg(target_os = "linux")]
fn quit_linux(appid: u32) -> anyhow::Result<bool> {
    let Some(pid) = reaper_pid_for_appid_linux(appid) else {
        return Ok(false);
    };
    // SIGTERM the whole process GROUP (`-pid`) so the game's child tree (Proton,
    // the game exe, helper processes) all receive it and shut down cleanly — this
    // is what Steam's Stop does. Graceful only: SIGTERM, never SIGKILL.
    //
    // Safety: `kill(2)` is an FFI call with no memory effects; we pass a negative
    // pid (the group) and the standard SIGTERM signal number.
    let rc = unsafe { libc::kill(-pid, libc::SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(anyhow::anyhow!(
            "kill(-{pid}, SIGTERM) for appid {appid} failed: {err}"
        ));
    }
    tracing::info!("steam-quit: sent SIGTERM to process group {pid} for appid {appid}");
    Ok(true)
}

/// Linux reaper-pid finder: scan `/proc/*/cmdline` for Steam's `reaper` launcher
/// whose argv matches `SteamLaunch AppId=<appid>`, returning that pid (the head of
/// the game's process group). Same `/proc` scan as [`running_appid_linux`], but
/// keyed to a specific appid and returning the pid rather than the appid.
/// Unreadable entries (permissions, pid-exit races) are skipped.
#[cfg(target_os = "linux")]
fn reaper_pid_for_appid_linux(appid: u32) -> Option<libc::pid_t> {
    let proc = std::fs::read_dir("/proc").ok()?;
    for dirent in proc.flatten() {
        // Only numeric (pid) directories carry a cmdline worth reading.
        let name = dirent.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = name.parse::<libc::pid_t>() else {
            continue;
        };
        let Ok(raw) = std::fs::read(dirent.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<&str> = raw
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect();
        if argv_matches_appid(&args, appid) {
            return Some(pid);
        }
    }
    None
}

/// Windows graceful-quit: resolve `appid`'s install directory, enumerate
/// running processes via a PowerShell CIM query, and `taskkill /PID <pid>`
/// (no `/F` — graceful window-close, never a hard kill) every process whose
/// executable path is unambiguously inside that install dir.
///
/// **Safety design**: we never touch a process unless BOTH (1) the appid's
/// install directory is resolved from its own `appmanifest_<appid>.acf` (never
/// guessed), and (2) the candidate process's executable path passes
/// [`exe_is_under_install_dir`]'s strict path-prefix-at-a-separator-boundary
/// check against that directory — a sibling directory sharing a name prefix
/// (`C:\Games\Foo` vs `C:\Games\FooBar`) can never match. If the install dir
/// can't be resolved, or no running process matches, we signal nothing and
/// return `Ok(false)` — the same clean "not running" result the Linux path
/// gives, never a guess.
///
/// Residual PID-reuse TOCTOU: there's a small window between the process
/// snapshot ([`running_processes_windows`]) and the `taskkill` where a matched
/// PID could exit and Windows recycle it onto an unrelated process. This is
/// inherent to PID-based `taskkill` without an `OpenProcess` handle held across
/// the check, which we can't do without a new crate — accepted as out of scope.
#[cfg(target_os = "windows")]
fn quit_windows(appid: u32) -> anyhow::Result<bool> {
    let Some(install_dir) = installdir_for_appid(appid) else {
        return Ok(false);
    };
    let install_dir = install_dir.to_string_lossy().to_string();

    let mut signalled = false;
    let procs = running_processes_windows();
    for (pid, exe) in processes_under_install_dir(&procs, &install_dir) {
        match std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .status()
        {
            Ok(status) if status.success() => {
                tracing::info!(
                    "steam-quit: taskkill'd pid {pid} ({exe}) for appid {appid} (install dir {install_dir})"
                );
                signalled = true;
            }
            Ok(status) => {
                tracing::debug!("steam-quit: taskkill pid {pid} exited {status}");
            }
            Err(e) => {
                tracing::debug!("steam-quit: taskkill pid {pid} failed to spawn: {e}");
            }
        }
    }
    Ok(signalled)
}

/// Pure filter: the subset of `procs` whose executable lives inside
/// `install_dir`, as `(pid, exe)` pairs borrowed from the input.
///
/// This is the **single** "is this appid's game actually running?" matcher on
/// Windows — both [`quit_windows`] (which taskkills the matches) and
/// [`appid_has_live_process`] (which only asks whether there are any) go through
/// it, so status and quit cannot draw different conclusions from the same
/// process list. Per-process membership is decided by
/// [`exe_is_under_install_dir`]'s strict boundary check. Pure (no I/O), so it is
/// unit-tested on every platform.
#[cfg(any(target_os = "windows", test))]
fn processes_under_install_dir<'a>(
    procs: &'a [(u32, String)],
    install_dir: &str,
) -> Vec<(u32, &'a str)> {
    procs
        .iter()
        .filter(|(_, exe)| exe_is_under_install_dir(exe, install_dir))
        .map(|(pid, exe)| (*pid, exe.as_str()))
        .collect()
}

/// Windows liveness cross-check: is a process for `appid` actually running?
///
/// Resolves the appid's install directory from its own `appmanifest_<appid>.acf`
/// (never guessed) and asks whether any running process's executable sits inside
/// it — the same two steps [`quit_windows`] takes before it signals anything,
/// via the same [`processes_under_install_dir`] matcher.
///
/// **Deliberately conservative**: every failure resolves to `false` (unresolvable
/// install dir, a process enumeration that returned nothing because PowerShell is
/// missing or the CIM query failed). A phantom "running" is the damaging error —
/// it badges a poster, restricts focus to that one card, and gates the context
/// menu — whereas a missed "running" only omits a badge. It also keeps this in
/// lockstep with quit: whenever this says "running", quit has something to kill,
/// and whenever this says "not running", quit would have found nothing anyway.
///
/// Two consequences of that conservatism, both accepted deliberately:
/// - A title whose real executable lives OUTSIDE the Steam install dir (a stub
///   manifest fronting a third-party launcher) reads as not running. That is
///   already true of [`quit_windows`], which likewise can't act on it — the two
///   agreeing is the point.
/// - `/sleep` consults [`running_appid`] for its refusal, so it will no longer
///   be blocked forever by a stale registry value. Its other guard (Sunshine's
///   live-session probe) is independent and unaffected.
///
/// **Cost**: one process enumeration (a PowerShell CIM query) per call, but only
/// when the registry claims a non-zero appid — an idle host short-circuits before
/// getting here and pays nothing, which is the overwhelmingly common state.
#[cfg(target_os = "windows")]
fn appid_has_live_process(appid: u32) -> bool {
    let Some(install_dir) = installdir_for_appid(appid) else {
        tracing::debug!(
            "steam: RunningAppID {appid} has no resolvable install dir; treating as not running"
        );
        return false;
    };
    let install_dir = install_dir.to_string_lossy().to_string();
    let procs = running_processes_windows();
    let matches = processes_under_install_dir(&procs, &install_dir);
    if matches.is_empty() {
        tracing::debug!(
            "steam: RunningAppID {appid} is stale — no live process under {install_dir}"
        );
        return false;
    }
    true
}

/// Resolve the absolute install directory for a Steam `appid` on Windows.
///
/// Scans every Steam library ([`library_folders`]) for
/// `steamapps/appmanifest_<appid>.acf`, reads its `installdir` field (pure
/// parse via [`parse_installdir`]), and joins it onto
/// `<library>/steamapps/common/` — the path Steam actually installs games
/// under. Returns `None` if no library has a manifest for `appid`.
#[cfg(target_os = "windows")]
fn installdir_for_appid(appid: u32) -> Option<PathBuf> {
    let steam_root = steam_roots().into_iter().find(|r| r.is_dir())?;
    for lib in library_folders(&steam_root) {
        let manifest = lib
            .join("steamapps")
            .join(format!("appmanifest_{appid}.acf"));
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        if let Some(installdir) = parse_installdir(&text) {
            return Some(lib.join("steamapps").join("common").join(installdir));
        }
    }
    None
}

/// Parse the `installdir` field out of an `appmanifest_*.acf` body — the
/// directory name (relative to `steamapps/common/`) the game is installed
/// under. Pure function (unit-tested against the checked-in fixture). `None`
/// when the field is absent, the VDF doesn't parse, OR the field is
/// empty/whitespace-only — a blank `installdir` would collapse the quit-safety
/// boundary to `steamapps\common\` (the parent of EVERY game), so it MUST be
/// rejected here, not treated as a valid relative path.
#[cfg(any(target_os = "windows", test))]
fn parse_installdir(acf: &str) -> Option<String> {
    let vdf = keyvalues_parser::parse(acf).map(Vdf::from).ok()?;
    let obj = vdf.value.get_obj()?;
    obj.get("installdir")
        .and_then(|vs| vs.first())
        .and_then(|v| v.get_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Enumerate running Windows processes as `(pid, executable_path)` pairs via a
/// PowerShell CIM query (`Get-CimInstance Win32_Process`) — no new crate, no
/// WMI bindings. Command/parse failures degrade to an empty list (the caller
/// then signals nothing, the safe default).
#[cfg(target_os = "windows")]
fn running_processes_windows() -> Vec<(u32, String)> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath } | ForEach-Object { \"$($_.ProcessId)`t$($_.ExecutablePath)\" }",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_process_list(&String::from_utf8_lossy(&output.stdout))
}

/// Pure parser: `PID\t<ExecutablePath>` lines (as emitted by the PowerShell
/// CIM query in [`running_processes_windows`]) → `(pid, path)` pairs.
/// Malformed lines (no tab, non-numeric pid, empty path) are skipped rather
/// than failing the whole scan. Unit-tested without touching a real process
/// list.
#[cfg(any(target_os = "windows", test))]
fn parse_process_list(stdout: &str) -> Vec<(u32, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let (pid, path) = line.trim_end().split_once('\t')?;
            let pid = pid.trim().parse::<u32>().ok()?;
            let path = path.trim();
            if path.is_empty() {
                return None;
            }
            Some((pid, path.to_string()))
        })
        .collect()
}

/// Pure predicate: is `exe_path` unambiguously located inside `install_dir`?
///
/// Normalizes both to lower-case with `\`-separators and no trailing
/// separator, then requires a true path-prefix match at a `\`-boundary —
/// `C:\Games\Foo` matches `C:\Games\Foo\bin\game.exe` but does **not** match
/// `C:\Games\FooBar\game.exe` (a sibling dir sharing a name prefix) or
/// `C:\Games\Foo` itself (not a file). This is the sole safety gate for
/// [`quit_windows`] — it is deliberately conservative: any ambiguity resolves
/// to "not under". A remainder containing a `..` (or `.`) component is also
/// rejected — `normalize` doesn't resolve `..`, so an exe path like
/// `C:\Games\Foo\..\..\Windows\System32\evil.exe` prefixes `c:\games\foo` yet
/// escapes it; requiring no traversal components closes that (defense-in-depth:
/// WMI `ExecutablePath` is normally already canonical). Unit-tested thoroughly
/// (nested exe, sibling-prefix rejection, exact-dir rejection,
/// case-insensitivity, `/` vs `\`, trailing separators, `..` escape).
#[cfg(any(target_os = "windows", test))]
fn exe_is_under_install_dir(exe_path: &str, install_dir: &str) -> bool {
    fn normalize(p: &str) -> String {
        let mut s = p.to_lowercase().replace('/', "\\");
        while s.ends_with('\\') {
            s.pop();
        }
        s
    }
    let exe = normalize(exe_path);
    let dir = normalize(install_dir);
    if dir.is_empty() || exe == dir {
        return false;
    }
    match exe.strip_prefix(&dir) {
        // The remainder must start at a path-separator boundary AND contain no
        // `..`/`.` traversal components (which `normalize` doesn't resolve).
        Some(rest) => {
            rest.starts_with('\\')
                && !rest
                    .split('\\')
                    .any(|component| component == ".." || component == ".")
        }
        None => false,
    }
}

/// Windows running-game detector: read the appid Steam *claims* is running (the
/// live registry DWORD via `reg query`, falling back to the on-disk
/// `registry.vdf` when the command fails outright, exits non-zero, or its output
/// doesn't parse), then **verify the claim against the process table** before
/// reporting it.
///
/// The verification is the whole point. `RunningAppID` is written when a game
/// starts and cleared when it stops *by a healthy Steam client* — a client that
/// crashes mid-game never clears it, so the value goes stale and the sidecar
/// reports a game that isn't running (observed: `RunningAppID = 252950` with no
/// `steam.exe` and an idle GPU). That phantom then drives the shell's poster
/// badge, its focus restriction, and its context-menu preconditions.
///
/// [`appid_has_live_process`] resolves the claim through the same install-dir +
/// process-match path [`quit_windows`] uses, so the two can no longer disagree.
/// Failure to verify reports "not running" (see that function's contract).
#[cfg(target_os = "windows")]
fn running_appid_windows() -> Option<u32> {
    let claimed = running_appid_reg_query().or_else(running_appid_registry)?;
    appid_has_live_process(claimed).then_some(claimed)
}

/// Shell out to `reg query "HKCU\Software\Valve\Steam" /v RunningAppID` and
/// parse its stdout. `None` on any failure (command missing, non-zero exit,
/// unparseable output) — the caller falls back to `registry.vdf`.
#[cfg(target_os = "windows")]
fn running_appid_reg_query() -> Option<u32> {
    let output = std::process::Command::new("reg")
        .args(["query", r"HKCU\Software\Valve\Steam", "/v", "RunningAppID"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_reg_query_dword(&String::from_utf8_lossy(&output.stdout))
}

/// Pure parser: `reg query`'s stdout for a single `REG_DWORD` value → the
/// parsed `u32`, or `None` for a missing/malformed value or a `0x0` value
/// (Steam writes `0x0` when nothing is running). Expected shape:
/// ```text
/// HKEY_CURRENT_USER\Software\Valve\Steam
///     RunningAppID    REG_DWORD    0x2d2
/// ```
/// Unit-tested against fixture stdout strings (set, zero, missing key, garbled).
#[cfg(any(target_os = "windows", test))]
fn parse_reg_query_dword(stdout: &str) -> Option<u32> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("RunningAppID"))?;
    let hex = line.split_whitespace().next_back()?;
    let hex = hex.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(hex, 16).ok().filter(|&id| id != 0)
}

/// Windows running-game detector (fallback path): read `RunningAppID` from
/// `registry.vdf` on disk. Used only when [`running_appid_reg_query`] fails —
/// Steam also maintains this file, so it degrades gracefully rather than
/// losing the signal entirely.
#[cfg(target_os = "windows")]
fn running_appid_registry() -> Option<u32> {
    let path = registry_vdf_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    parse_running_appid(&text)
}

/// Resolve the path to Steam's `registry.vdf` (Windows running-game path only).
#[cfg(target_os = "windows")]
fn registry_vdf_path() -> Option<PathBuf> {
    // Test/override hook: point at a fixture.
    if let Some(p) = tv_shell_protocol::brand::env("STEAM_REGISTRY") {
        return Some(PathBuf::from(p));
    }
    // Default install; a STEAM_PATH override refines this. registry.vdf sits at
    // the Steam root.
    let root = std::env::var("STEAM_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Program Files (x86)\Steam"));
    Some(root.join("registry.vdf"))
}

/// Parse the `RunningAppID` out of a `registry.vdf` body (Windows source only).
/// Pure function (unit-tested). The file is Valve KeyValues:
/// `"Registry" { "HKCU" { "Software" { "Valve" { "Steam" { "RunningAppID" "<n>" … } } } } }`.
/// Returns `None` for a missing key or a `0`/non-numeric value.
#[cfg(any(target_os = "windows", test))]
pub fn parse_running_appid(text: &str) -> Option<u32> {
    let vdf = keyvalues_parser::parse(text).map(Vdf::from).ok()?;
    // Walk the fixed nesting Registry > HKCU > Software > Valve > Steam.
    let mut obj = vdf.value.get_obj()?;
    for key in ["HKCU", "Software", "Valve", "Steam"] {
        obj = obj.get(key).and_then(|vs| vs.first())?.get_obj()?;
    }
    let raw = obj
        .get("RunningAppID")
        .and_then(|vs| vs.first())
        .and_then(|v| v.get_str())?;
    raw.trim().parse::<u32>().ok().filter(|&id| id != 0)
}

/// Whether a Moonlight/Sunshine session is currently live on this host —
/// **active OR resumable** (suspended).
///
/// The signal is Sunshine's own GameStream `serverinfo` state, the same thing the
/// Moonlight client reads: `<state>SUNSHINE_SERVER_BUSY</state>` means an app
/// session is running and streamable — whether a client is actively connected
/// (encoding) or the session is paused/resumable after a client disconnected.
/// `…_FREE` means idle.
///
/// This deliberately replaces the earlier NVENC encode-session probe, which only
/// saw an *actively encoding* stream and reported `false` for a resumable session
/// the user can still reconnect to — the mismatch the session indicator showed
/// ("No session" while the laptop's Moonlight still listed BPM as resumable). It
/// is also GPU-agnostic (no `nvidia-smi`), so it works on AMD/Intel hosts.
///
/// Best-effort: a minimal sync HTTP GET to Sunshine's HTTP port (default 47989,
/// override via `TV_SHELL_SUNSHINE_PORT`) on loopback — the *unpaired*
/// `serverinfo` returns basic state (incl. `<state>`) with no client cert.
/// Sunshine down / unreachable / any error ⇒ `false` (never fail the endpoint).
pub fn streaming() -> bool {
    sunshine_server_busy()
}

/// Sunshine GameStream HTTP port. Default 47989 (Sunshine's GameStream base);
/// override with `TV_SHELL_SUNSHINE_PORT` for a non-default install.
fn sunshine_http_port() -> u16 {
    tv_shell_protocol::brand::env("SUNSHINE_PORT")
        .and_then(|p| p.trim().parse::<u16>().ok())
        .unwrap_or(47989)
}

/// Query Sunshine's unpaired `serverinfo` on loopback and report whether a
/// session is live (active or resumable). Dependency-free sync HTTP/1.0 GET (this
/// runs inside `spawn_blocking`), so the host crate stays HTTP-client-free.
/// Any connect/read failure ⇒ `false`.
fn sunshine_server_busy() -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let addr = format!("127.0.0.1:{}", sunshine_http_port());
    let Ok(mut stream) = TcpStream::connect(&addr) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    // The `uniqueid` is required by the GameStream contract but any value works
    // for the unpaired basic info.
    let req = "GET /serverinfo?uniqueid=0123456789ABCDEF HTTP/1.0\r\n\
               Host: localhost\r\nConnection: close\r\n\r\n";
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }

    // Read incrementally rather than `read_to_string`: Sunshine may keep the
    // connection alive (ignoring `Connection: close`), in which case a blocking
    // read-to-EOF would just hit the timeout and yield nothing. Instead accumulate
    // chunks and stop as soon as the answer is decidable — the `BUSY` token
    // appears, or the closing `</root>` shows the full body arrived — so neither a
    // busy nor an idle response waits for the timeout. On EOF/timeout we evaluate
    // whatever arrived.
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break, // EOF — server closed
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let seen = String::from_utf8_lossy(&buf);
                if seen.contains("SUNSHINE_SERVER_BUSY") {
                    return true;
                }
                // Full body in hand (idle case) — no need to wait for the timeout.
                if seen.contains("</root>") || buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break, // timeout or error — evaluate what we have
        }
    }
    serverinfo_is_busy(&String::from_utf8_lossy(&buf))
}

/// True when a Sunshine `serverinfo` response reports a live (active or
/// resumable) session: `<state>SUNSHINE_SERVER_BUSY</state>`. Idle is
/// `…_SERVER_FREE`. Pure function (unit-tested).
fn serverinfo_is_busy(body: &str) -> bool {
    body.contains("SUNSHINE_SERVER_BUSY")
}

/// Candidate art filenames inside an appid's `librarycache/<appid>/<hash>/` dir
/// (modern layout), in preference order: the 300×450 portrait capsule poster
/// first, then the 460×215 header as a fallback.
const ART_CAPSULE: &str = "library_capsule.jpg";
const ART_HEADER: &str = "library_header.jpg";

/// Legacy (pre-hash-subdir) Steam clients instead wrote flat files directly
/// under `librarycache/`, named `<appid>_library_600x900.jpg` (portrait) and
/// `<appid>_header.jpg`. These are the filename *suffixes* appended to the
/// appid; `pick_art_file` treats them as its own preference tier below the
/// modern names.
const ART_FLAT_LIBRARY: &str = "_library_600x900.jpg";
const ART_FLAT_HEADER: &str = "_header.jpg";

/// Resolve the local portrait art file for a Steam `appid` from the on-disk
/// library cache, or `None` when nothing is cached (or Steam isn't installed).
///
/// Steam stores art at
/// `<steam_root>/appcache/librarycache/<appid>/<hash>/library_capsule.jpg`,
/// where `<hash>` is an unpredictable 40-char hex subdir (there can be several
/// per appid). This relative `appcache/librarycache/...` layout is identical
/// on every OS Steam supports — `steam_root` is resolved per-OS via
/// [`steam_roots`] (Windows: `STEAM_PATH` override or the default Program
/// Files install), so this modern-layout path needs no Windows-specific
/// handling; only the root itself differs.
///
/// We scan the appid dir's immediate subdirectories for the capsule
/// (preferred) or header (fallback). If neither is found (e.g. an older Steam
/// client that never migrated to the per-appid/hash cache layout), we fall
/// back to the flat `<appid>_library_600x900.jpg` / `<appid>_header.jpg` files
/// directly under `librarycache/` — checked only in that fallback case, so a
/// modern install never pays the extra stat calls. The Steam root is resolved
/// exactly like [`enumerate`] (first existing [`steam_roots`] dir).
pub fn library_art_path(appid: u32) -> Option<PathBuf> {
    let steam_root = steam_roots().into_iter().find(|r| r.is_dir())?;
    let librarycache = steam_root.join("appcache/librarycache");
    let appid_dir = librarycache.join(appid.to_string());

    // Gather the art filenames present across the immediate subdirs, paired with
    // the absolute path they live at, so the pure picker decides which to use.
    let mut found: Vec<(&'static str, PathBuf)> = Vec::new();
    if let Ok(read) = std::fs::read_dir(&appid_dir) {
        for dirent in read.flatten() {
            let sub = dirent.path();
            if !sub.is_dir() {
                continue;
            }
            for name in [ART_CAPSULE, ART_HEADER] {
                let candidate = sub.join(name);
                if candidate.is_file() {
                    found.push((name, candidate));
                }
            }
        }
    }

    if found.is_empty() {
        for name in [ART_FLAT_LIBRARY, ART_FLAT_HEADER] {
            let candidate = librarycache.join(format!("{appid}{name}"));
            if candidate.is_file() {
                found.push((name, candidate));
            }
        }
    }

    let names: Vec<&str> = found.iter().map(|(n, _)| *n).collect();
    let pick = pick_art_file(&names)?;
    found
        .into_iter()
        .find(|(n, _)| *n == pick)
        .map(|(_, path)| path)
}

/// Pure picker: given the art filenames present across an appid's cache dirs,
/// pick the most-preferred one, or `None` if none of the known names are
/// present. Preference order: modern capsule, modern header, legacy flat
/// library, legacy flat header. Factored out of [`library_art_path`] so it's
/// unit-testable without touching the filesystem.
fn pick_art_file<'a>(names: &[&'a str]) -> Option<&'a str> {
    [ART_CAPSULE, ART_HEADER, ART_FLAT_LIBRARY, ART_FLAT_HEADER]
        .into_iter()
        .find(|candidate| names.contains(candidate))
}

/// Is this entry Steam runtime tooling (Proton, Linux Runtime, redistributables)
/// rather than a real game? Matches known appids and lower-cased name prefixes.
fn is_runtime(entry: &LibraryEntry) -> bool {
    if RUNTIME_APPIDS.contains(&entry.appid) {
        return true;
    }
    let name = entry.name.to_lowercase();
    RUNTIME_NAME_PREFIXES.iter().any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed but realistic appmanifest_*.acf (Valve KeyValues).
    const FIXTURE_ACF: &str = include_str!("../tests/fixtures/appmanifest_730.acf");

    // A realistic registry.vdf with RunningAppID set (Valve KeyValues).
    const FIXTURE_REGISTRY: &str = include_str!("../tests/fixtures/registry.vdf");

    #[test]
    fn parses_fixture_manifest() {
        let entry = parse_appmanifest(FIXTURE_ACF).expect("fixture parses");
        assert_eq!(entry.appid, 730);
        assert_eq!(entry.name, "Counter-Strike 2");
        assert_eq!(entry.last_played, Some(1_700_000_000));
        assert_eq!(entry.size_on_disk, Some(35_000_000_000));
        assert!(entry.installed); // StateFlags 4 → fully installed.
    }

    #[test]
    fn partially_installed_flagged() {
        let acf = r#""AppState"
{
    "appid"  "12345"
    "name"   "Half-Downloaded"
    "StateFlags"  "1026"
    "SizeOnDisk"  "100"
}"#;
        let entry = parse_appmanifest(acf).unwrap();
        assert_eq!(entry.appid, 12345);
        // 1026 & 4 == 0 → not fully installed.
        assert!(!entry.installed);
    }

    #[test]
    fn missing_appid_is_none() {
        let acf = r#""AppState" { "name" "No Appid" }"#;
        assert!(parse_appmanifest(acf).is_none());
    }

    #[test]
    fn zero_last_played_is_none() {
        let acf = r#""AppState"
{
    "appid"  "55"
    "name"   "Never Played"
    "LastPlayed"  "0"
    "StateFlags"  "4"
}"#;
        let entry = parse_appmanifest(acf).unwrap();
        assert_eq!(entry.last_played, None);
    }

    #[test]
    fn parses_library_folders_paths() {
        let vdf = r#""libraryfolders"
{
    "0"
    {
        "path"   "/home/user/.local/share/Steam"
        "label"  ""
    }
    "1"
    {
        "path"   "/mnt/games/SteamLibrary"
        "label"  ""
    }
}"#;
        let paths = parse_library_folders(vdf);
        assert_eq!(
            paths,
            vec![
                "/home/user/.local/share/Steam".to_string(),
                "/mnt/games/SteamLibrary".to_string(),
            ]
        );
    }

    #[test]
    fn parses_running_appid_from_fixture() {
        // The fixture has RunningAppID 730 nested under Registry/HKCU/Software/Valve/Steam.
        assert_eq!(parse_running_appid(FIXTURE_REGISTRY), Some(730));
    }

    #[test]
    fn running_appid_zero_is_none() {
        let vdf = r#""Registry"
{
    "HKCU"
    {
        "Software"
        {
            "Valve"
            {
                "Steam"
                {
                    "RunningAppID"  "0"
                }
            }
        }
    }
}"#;
        assert_eq!(parse_running_appid(vdf), None);
    }

    #[test]
    fn running_appid_missing_key_is_none() {
        let vdf = r#""Registry"
{
    "HKCU"
    {
        "Software"
        {
            "Valve"
            {
                "Steam"
                {
                    "SomethingElse"  "1"
                }
            }
        }
    }
}"#;
        assert_eq!(parse_running_appid(vdf), None);
    }

    #[test]
    fn running_appid_unparseable_is_none() {
        assert_eq!(parse_running_appid("not vdf at all {{{"), None);
    }

    #[test]
    fn steam_appid_from_reaper_argv() {
        // The canonical Linux launcher process for a running game.
        let argv = [
            "reaper",
            "SteamLaunch",
            "AppId=2215200",
            "--",
            "/home/user/.steam/steam/steamapps/common/Proton/proton",
            "waitforexitandrun",
            "/path/to/game.exe",
        ];
        assert_eq!(steam_appid_from_argv(&argv), Some(2215200));
    }

    #[test]
    fn steam_appid_from_unrelated_argv_is_none() {
        let argv = ["/usr/bin/firefox", "--new-window", "https://example.com"];
        assert_eq!(steam_appid_from_argv(&argv), None);
    }

    #[test]
    fn steam_appid_missing_appid_is_none() {
        // SteamLaunch present but no AppId= arg.
        let argv = ["reaper", "SteamLaunch", "--", "/some/proton"];
        assert_eq!(steam_appid_from_argv(&argv), None);
    }

    #[test]
    fn steam_appid_garbled_appid_is_none() {
        // SteamLaunch present, AppId= present but non-numeric.
        let argv = ["reaper", "SteamLaunch", "AppId=notanumber", "--"];
        assert_eq!(steam_appid_from_argv(&argv), None);
    }

    #[test]
    fn argv_matches_appid_only_for_that_game() {
        let argv = [
            "reaper",
            "SteamLaunch",
            "AppId=2215200",
            "--",
            "/path/to/game.exe",
        ];
        // Matches its own appid, nothing else.
        assert!(argv_matches_appid(&argv, 2215200));
        assert!(!argv_matches_appid(&argv, 730));
        // A non-launcher argv never matches any appid.
        let unrelated = ["/usr/bin/firefox", "--new-window"];
        assert!(!argv_matches_appid(&unrelated, 2215200));
        // SteamLaunch with no AppId= matches nothing.
        let no_id = ["reaper", "SteamLaunch", "--", "/some/proton"];
        assert!(!argv_matches_appid(&no_id, 2215200));
    }

    #[test]
    fn serverinfo_busy_detected() {
        // A live (active or resumable) session: state BUSY.
        let busy =
            "<root><state>SUNSHINE_SERVER_BUSY</state><currentgame>1093255277</currentgame></root>";
        assert!(serverinfo_is_busy(busy));
    }

    #[test]
    fn serverinfo_free_is_not_busy() {
        // Idle: state FREE, no current game.
        let free = "<root><state>SUNSHINE_SERVER_FREE</state><currentgame>0</currentgame></root>";
        assert!(!serverinfo_is_busy(free));
        // Empty / failed fetch ⇒ not busy.
        assert!(!serverinfo_is_busy(""));
        assert!(!serverinfo_is_busy("HTTP/1.0 404 Not Found"));
    }

    #[test]
    fn pick_art_prefers_capsule_over_header() {
        // Both present (possibly in different subdirs) → capsule wins.
        assert_eq!(
            pick_art_file(&["library_header.jpg", "library_capsule.jpg"]),
            Some("library_capsule.jpg")
        );
    }

    #[test]
    fn pick_art_falls_back_to_header() {
        assert_eq!(
            pick_art_file(&["library_header.jpg"]),
            Some("library_header.jpg")
        );
    }

    #[test]
    fn pick_art_none_when_absent() {
        assert_eq!(pick_art_file(&[]), None);
        assert_eq!(pick_art_file(&["icon.jpg", "logo.png"]), None);
    }

    #[test]
    fn pick_art_prefers_flat_library_over_flat_header() {
        // Legacy flat layout: both present → the portrait one wins, same
        // preference shape as the modern capsule/header pair.
        assert_eq!(
            pick_art_file(&["_header.jpg", "_library_600x900.jpg"]),
            Some("_library_600x900.jpg")
        );
    }

    #[test]
    fn pick_art_falls_back_to_flat_header() {
        assert_eq!(pick_art_file(&["_header.jpg"]), Some("_header.jpg"));
    }

    #[test]
    fn pick_art_modern_beats_legacy_flat_when_both_somehow_present() {
        // Shouldn't happen in practice (library_art_path only checks flat names
        // when the modern scan found nothing), but the pure picker's preference
        // order should still favor the modern names if ever handed both.
        assert_eq!(
            pick_art_file(&["_library_600x900.jpg", "library_header.jpg"]),
            Some("library_header.jpg")
        );
    }

    #[test]
    fn parses_installdir_from_fixture() {
        assert_eq!(
            parse_installdir(FIXTURE_ACF).as_deref(),
            Some("Counter-Strike Global Offensive")
        );
    }

    #[test]
    fn parse_installdir_missing_field_is_none() {
        let acf = r#""AppState" { "appid" "42" "name" "No Installdir" }"#;
        assert_eq!(parse_installdir(acf), None);
    }

    #[test]
    fn parse_installdir_unparseable_is_none() {
        assert_eq!(parse_installdir("not vdf at all {{{"), None);
    }

    #[test]
    fn parse_installdir_empty_field_is_none() {
        // A blank / whitespace-only installdir MUST NOT resolve — otherwise the
        // quit-safety boundary collapses to `steamapps\common\` (every game's
        // parent) and quit_windows could taskkill unrelated games.
        assert_eq!(
            parse_installdir(r#""AppState" { "appid" "42" "installdir" "" }"#),
            None
        );
        assert_eq!(
            parse_installdir(r#""AppState" { "appid" "42" "installdir" "   " }"#),
            None
        );
    }

    #[test]
    fn reg_query_dword_parses_set_value() {
        let stdout = "\r\nHKEY_CURRENT_USER\\Software\\Valve\\Steam\r\n    RunningAppID    REG_DWORD    0x2d2\r\n\r\n";
        assert_eq!(parse_reg_query_dword(stdout), Some(0x2d2));
    }

    #[test]
    fn reg_query_dword_zero_is_none() {
        let stdout =
            "HKEY_CURRENT_USER\\Software\\Valve\\Steam\r\n    RunningAppID    REG_DWORD    0x0\r\n";
        assert_eq!(parse_reg_query_dword(stdout), None);
    }

    #[test]
    fn reg_query_dword_missing_key_is_none() {
        // `reg query` prints this to stdout/stderr when the value doesn't exist;
        // either way there's no `RunningAppID` line to find.
        let stdout =
            "ERROR: The system was unable to find the specified registry key or value.\r\n";
        assert_eq!(parse_reg_query_dword(stdout), None);
    }

    #[test]
    fn reg_query_dword_garbled_is_none() {
        assert_eq!(parse_reg_query_dword("not reg output at all {{{"), None);
        assert_eq!(
            parse_reg_query_dword("RunningAppID    REG_DWORD    notahexvalue"),
            None
        );
    }

    #[test]
    fn parse_process_list_extracts_pid_and_path() {
        let stdout = "1234\tC:\\Games\\Foo\\game.exe\r\n5678\tC:\\Windows\\explorer.exe\r\n";
        assert_eq!(
            parse_process_list(stdout),
            vec![
                (1234, r"C:\Games\Foo\game.exe".to_string()),
                (5678, r"C:\Windows\explorer.exe".to_string()),
            ]
        );
    }

    #[test]
    fn parse_process_list_skips_malformed_lines() {
        let stdout = "not a line\r\n\r\nnotanumber\tC:\\Foo\\bar.exe\r\n42\t\r\n99\tC:\\ok.exe\r\n";
        assert_eq!(
            parse_process_list(stdout),
            vec![(99, "C:\\ok.exe".to_string())]
        );
    }

    #[test]
    fn exe_under_install_dir_nested_exe_matches() {
        assert!(exe_is_under_install_dir(
            r"C:\Games\Foo\bin\game.exe",
            r"C:\Games\Foo"
        ));
    }

    #[test]
    fn exe_under_install_dir_sibling_prefix_does_not_match() {
        // FooBar shares the "Foo" prefix but is a different directory.
        assert!(!exe_is_under_install_dir(
            r"C:\Games\FooBar\game.exe",
            r"C:\Games\Foo"
        ));
    }

    #[test]
    fn exe_under_install_dir_exact_dir_does_not_match() {
        // The install dir itself is not a file/exe path.
        assert!(!exe_is_under_install_dir(r"C:\Games\Foo", r"C:\Games\Foo"));
    }

    #[test]
    fn exe_under_install_dir_case_insensitive() {
        assert!(exe_is_under_install_dir(
            r"c:\games\foo\game.exe",
            r"C:\Games\Foo"
        ));
    }

    #[test]
    fn exe_under_install_dir_handles_forward_slashes_and_trailing_separator() {
        assert!(exe_is_under_install_dir(
            "C:/Games/Foo/bin/game.exe",
            r"C:\Games\Foo\"
        ));
    }

    #[test]
    fn exe_under_install_dir_unrelated_paths_do_not_match() {
        assert!(!exe_is_under_install_dir(
            r"C:\Windows\explorer.exe",
            r"C:\Games\Foo"
        ));
    }

    #[test]
    fn exe_under_install_dir_rejects_dotdot_escape() {
        // `normalize` doesn't resolve `..`, so this path string-prefixes
        // `c:\games\foo` yet actually escapes to System32 — must be rejected.
        assert!(!exe_is_under_install_dir(
            r"C:\Games\Foo\..\..\Windows\System32\evil.exe",
            r"C:\Games\Foo"
        ));
        // A single-dot component is likewise rejected as non-canonical.
        assert!(!exe_is_under_install_dir(
            r"C:\Games\Foo\.\game.exe",
            r"C:\Games\Foo"
        ));
    }

    /// Table-driven cases for the liveness matcher shared by `quit_windows` and
    /// `appid_has_live_process`: a realistic process list, the install dir under
    /// test, and the pids expected to match.
    #[test]
    fn processes_under_install_dir_table() {
        let procs = vec![
            (4, "C:\\Windows\\System32\\smss.exe".to_string()),
            (100, "C:\\Program Files (x86)\\Steam\\steam.exe".to_string()),
            (
                200,
                "D:\\SteamLibrary\\steamapps\\common\\rocketleague\\Binaries\\Win64\\RocketLeague.exe".to_string(),
            ),
            (
                201,
                "D:\\SteamLibrary\\steamapps\\common\\rocketleague\\bootstrapper.exe".to_string(),
            ),
            (
                300,
                "D:\\SteamLibrary\\steamapps\\common\\rocketleague2\\game.exe".to_string(),
            ),
        ];
        let cases: &[(&str, &[u32])] = &[
            // The game's own tree — both processes under the install dir match.
            (
                r"D:\SteamLibrary\steamapps\common\rocketleague",
                &[200, 201],
            ),
            // Trailing separator + forward slashes normalize to the same dir.
            (
                "D:/SteamLibrary/steamapps/common/RocketLeague/",
                &[200, 201],
            ),
            // A sibling sharing a name prefix must NOT drag in the other game.
            (r"D:\SteamLibrary\steamapps\common\rocketleague2", &[300]),
            // An installed-but-not-running game matches nothing.
            (r"D:\SteamLibrary\steamapps\common\portal2", &[]),
            // An empty install dir must never match everything.
            ("", &[]),
        ];
        for (dir, expected) in cases {
            let got: Vec<u32> = processes_under_install_dir(&procs, dir)
                .into_iter()
                .map(|(pid, _)| pid)
                .collect();
            assert_eq!(&got, expected, "install_dir = {dir:?}");
        }
    }

    #[test]
    fn processes_under_install_dir_empty_process_list_matches_nothing() {
        // The enumeration degrading to an empty list (PowerShell missing, CIM
        // query failed) must read as "not running", never as "can't tell, assume
        // running" — a phantom running game is the damaging direction.
        assert!(
            processes_under_install_dir(&[], r"D:\SteamLibrary\steamapps\common\rocketleague")
                .is_empty()
        );
    }

    #[test]
    fn processes_under_install_dir_borrows_exe_paths() {
        // The matcher hands back the exe path so quit_windows can keep logging
        // WHAT it killed, not just the pid.
        let procs = vec![(7, r"C:\Games\Foo\bin\game.exe".to_string())];
        assert_eq!(
            processes_under_install_dir(&procs, r"C:\Games\Foo"),
            vec![(7, r"C:\Games\Foo\bin\game.exe")]
        );
    }

    #[test]
    fn runtime_junk_is_detected() {
        let proton = LibraryEntry {
            appid: 999999,
            name: "Proton 9.0".to_string(),
            last_played: None,
            size_on_disk: None,
            installed: true,
        };
        assert!(is_runtime(&proton));

        let redist = LibraryEntry {
            appid: 228980,
            name: "Steamworks Common Redistributables".to_string(),
            last_played: None,
            size_on_disk: None,
            installed: true,
        };
        assert!(is_runtime(&redist));

        let game = LibraryEntry {
            appid: 730,
            name: "Counter-Strike 2".to_string(),
            last_played: None,
            size_on_disk: None,
            installed: true,
        };
        assert!(!is_runtime(&game));
    }
}
