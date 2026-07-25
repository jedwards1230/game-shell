//! Host power control — suspend the gaming PC on request.
//!
//! The TV client's Power overlay can put the *streaming host* to sleep, not just
//! the TV box. That request lands on `POST /sleep` (see `main.rs`), which asks
//! [`suspend_refusal`] whether suspending is safe right now and only then calls
//! [`suspend`].
//!
//! **Why a refusal exists at all**: this machine is not a single-purpose games
//! console. Besides Steam it hosts Sunshine (so another Moonlight client may be
//! mid-session) and long-running background services. Suspending out from under
//! any of those loses work, so the host decides — the caller only asks.
//!
//! Same shape as the rest of the crate: the *decision* is a pure function
//! ([`suspend_refusal`]) that unit-tests on every platform, and the OS glue is
//! one public fn with `#[cfg]` arms inside its body (mirroring
//! [`crate::steam::quit`] / [`crate::steam::running_appid`]). No new crates —
//! Windows integration is a shell-out, per the crate's pure-Rust dep policy.

// Only the Linux/Windows arms shell out; on any other OS `suspend` short-circuits
// to an "unsupported" error and this import would be dead.
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::process::Command;

/// Refusal reason when a Steam game is running on the host.
const REASON_GAME_RUNNING: &str = "a game is running on the host";

/// Refusal reason when Sunshine reports a live (active or resumable) session.
const REASON_STREAMING: &str = "a streaming session is active on the host";

/// Should a suspend request be refused? Returns `Some(reason)` when the host must
/// stay awake and `None` when it may sleep.
///
/// Both inputs come from the signals `/status` already publishes:
/// - `running_appid` — [`crate::steam::running_appid`], the foreground Steam game
///   (`None` ⇒ nothing running).
/// - `streaming` — [`crate::steam::streaming`], Sunshine's GameStream
///   `serverinfo` reporting `SUNSHINE_SERVER_BUSY`, i.e. a session that is active
///   **or merely resumable**. A resumable session counts: suspending would strand
///   a client that still lists this host as reconnectable.
///
/// **Precedence when both are true: the running game wins.** The reasons are
/// surfaced to a human, and "a game is running" is both the more specific and the
/// more actionable message (quit the game vs. hunt for a stream that is only
/// there *because* of the game). Deterministic and unit-tested so the message
/// never depends on probe ordering.
///
/// Pure — no I/O, no OS calls — so the whole truth table is unit-tested.
pub fn suspend_refusal(running_appid: Option<u32>, streaming: bool) -> Option<&'static str> {
    if running_appid.is_some() {
        return Some(REASON_GAME_RUNNING);
    }
    if streaming {
        return Some(REASON_STREAMING);
    }
    None
}

/// Suspend the host to RAM (S3).
///
/// Returns `Ok(())` once the OS suspend command has been **spawned**, not once
/// the machine is asleep — see [`suspend_command`] for why we never wait on the
/// child. An `Err` therefore means only "we could not even start the suspend"
/// (command missing, spawn refused), which is exactly the failure a caller can
/// act on.
///
/// - **Linux**: `systemctl suspend`.
/// - **Windows**: PowerShell + an inline `Add-Type` P/Invoke to .NET
///   `Application.SetSuspendState` — see [`suspend_command`] for the rundll32
///   trap this deliberately avoids.
/// - **Other OSes** (macOS): not wired — returns an "unsupported" `Err`, the same
///   way [`crate::steam::quit`] degrades into a clean not-supported answer rather
///   than pretending it worked.
pub fn suspend() -> anyhow::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let mut cmd = suspend_command();
        let child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn suspend command: {e}"))?;
        tracing::info!("sleep: suspend command dispatched");
        // Deliberately do NOT wait on the child — see `suspend_command`.
        drop(child);
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        // macOS (and any other OS): no suspend path wired. `pmset sleepnow` would
        // work but this sidecar's suspend feature targets the Linux/Windows
        // dual-boot gaming PC, and silently sleeping a dev Mac is not a behaviour
        // to add untested. Mirrors how `steam::quit` declines off Linux/Windows.
        Err(anyhow::anyhow!(
            "suspend is not supported on this operating system"
        ))
    }
}

/// Build the per-OS suspend command.
///
/// We spawn it and never `wait()`, for one reason per platform:
/// - **Windows**: `Application.SetSuspendState` BLOCKS until the machine
///   *resumes*, so waiting would pin the handler (and a blocking-pool thread) for
///   however many hours the box stays asleep.
/// - **Linux**: `systemctl suspend` returns quickly, but the kernel can freeze
///   the process mid-`wait()`, so the HTTP response would never flush.
///
/// ## Windows: why NOT `rundll32 powrprof.dll,SetSuspendState`
///
/// The commonly-copied one-liner `rundll32.exe powrprof.dll,SetSuspendState 0,1,0`
/// **ignores its arguments**. Whenever hibernation is enabled on the machine —
/// and it IS enabled on the target box — that entry point HIBERNATES (S4) instead
/// of suspending (S3), regardless of what you pass it. This has been verified on
/// the target machine. Hibernate is the wrong behaviour here: it is slow to
/// resume and interacts badly with Wake-on-LAN.
///
/// The .NET `System.Windows.Forms.Application.SetSuspendState(PowerState.Suspend,
/// false, false)` API honours its `PowerState` argument and produces a true S3
/// suspend; it has been verified working on the target machine. Reaching it needs
/// no new crate — `powershell -Command` with an inline `Add-Type` is a plain
/// shell-out, keeping the crate's "pure-Rust, cross-platform graph only" dep
/// policy intact (there is deliberately no `windows-sys`/`winapi` dependency).
///
/// **Do not "simplify" this back to rundll32.** It looks shorter and it silently
/// does the wrong thing.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn suspend_command() -> Command {
    #[cfg(target_os = "linux")]
    {
        let mut c = Command::new("systemctl");
        c.arg("suspend");
        c
    }
    #[cfg(target_os = "windows")]
    {
        let mut c = Command::new("powershell");
        c.args(windows_suspend_args());
        c
    }
}

/// The full PowerShell argv for the Windows arm of [`suspend_command`]. Split out
/// (and compiled under `test` on every platform) so the Windows-only argument
/// list is still exercised by the suite on a Linux/macOS developer machine, where
/// the `target_os = "windows"` block itself never compiles.
#[cfg(any(target_os = "windows", test))]
fn windows_suspend_args() -> [&'static str; 4] {
    // `-NoProfile`/`-NonInteractive`: never load a user profile or block on a
    // prompt — this runs from a service with no console attached.
    ["-NoProfile", "-NonInteractive", "-Command", WINDOWS_SUSPEND]
}

/// The PowerShell snippet behind the Windows arm of [`suspend_command`]:
/// load `System.Windows.Forms` and call `SetSuspendState(Suspend, force: false,
/// disableWakeEvent: false)`.
///
/// `disableWakeEvent: false` matters — it leaves wake events (including
/// Wake-on-LAN) armed, which is the whole point of a box the TV client is
/// expected to wake again later.
#[cfg(any(target_os = "windows", test))]
const WINDOWS_SUSPEND: &str = "Add-Type -AssemblyName System.Windows.Forms; \
     [System.Windows.Forms.Application]::SetSuspendState('Suspend', $false, $false)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_refusal_when_idle() {
        assert_eq!(suspend_refusal(None, false), None);
    }

    #[test]
    fn refuses_when_a_game_is_running() {
        assert_eq!(suspend_refusal(Some(730), false), Some(REASON_GAME_RUNNING));
        // Any appid refuses, including a small/unusual one.
        assert_eq!(suspend_refusal(Some(1), false), Some(REASON_GAME_RUNNING));
    }

    #[test]
    fn refuses_when_streaming() {
        assert_eq!(suspend_refusal(None, true), Some(REASON_STREAMING));
    }

    #[test]
    fn running_game_takes_precedence_over_streaming() {
        // Both true — the documented deterministic winner is the running game
        // (more specific + more actionable than "a stream is live", which is
        // usually live *because* of that game).
        assert_eq!(suspend_refusal(Some(730), true), Some(REASON_GAME_RUNNING));
    }

    #[test]
    fn refusal_reasons_are_distinct_and_human_readable() {
        assert_ne!(REASON_GAME_RUNNING, REASON_STREAMING);
        for reason in [REASON_GAME_RUNNING, REASON_STREAMING] {
            assert!(!reason.is_empty());
            // Sentence-ish, not a machine token — these are shown to a person.
            assert!(reason.contains(' '), "{reason:?} should read as prose");
        }
    }

    #[test]
    fn windows_snippet_uses_dotnet_setsuspendstate_not_rundll32() {
        // Guard the trap documented on `suspend_command`: rundll32's
        // powrprof.dll,SetSuspendState ignores its args and HIBERNATES whenever
        // hibernation is enabled. If someone "simplifies" the Windows arm back to
        // it, this fails.
        assert!(!WINDOWS_SUSPEND.contains("rundll32"));
        assert!(!WINDOWS_SUSPEND.contains("powrprof"));
        assert!(WINDOWS_SUSPEND.contains("SetSuspendState"));
        assert!(WINDOWS_SUSPEND.contains("System.Windows.Forms"));
        // Suspend (S3), and wake events left armed so WoL can bring it back.
        assert!(WINDOWS_SUSPEND.contains("'Suspend'"));
        assert!(WINDOWS_SUSPEND.contains("$false, $false"));
    }

    #[test]
    fn windows_suspend_argv_is_noninteractive_and_profile_free() {
        // Compiled on every platform (see `windows_suspend_args`), so the
        // Windows-only argv is covered even on a macOS/Linux dev box.
        let args = windows_suspend_args();
        assert_eq!(args[0], "-NoProfile");
        assert_eq!(args[1], "-NonInteractive");
        assert_eq!(args[2], "-Command");
        assert_eq!(args[3], WINDOWS_SUSPEND);
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn suspend_command_is_built_without_spawning() {
        // Build-only: assert the command has a program and carries our arguments.
        // We never spawn it in a test — that would suspend the machine running CI.
        let cmd = suspend_command();
        let prog = cmd.get_program().to_string_lossy().to_string();
        assert!(!prog.is_empty());
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(!args.is_empty(), "suspend command should carry arguments");
    }
}
