# Kiosk Window Model

How tv-shell guarantees the kiosk contract on Hyprland, why the previous
reactive approach kept failing, and how the current implementation makes it
structural.

**The contract.** Exactly one app window is visible and fills the screen; the
shell (Quickshell) sits deterministically above/below it; backgrounded apps
(Plex HTPC, Steam) keep running but never share the screen. Two app windows must
never be visible at once.

**Assumptions.** Hyprland ~0.55 (config targets 0.55). The Quickshell shell is a
**layer-shell** surface (`WlrLayershell`, holds `WlrKeyboardFocus.Exclusive`
while mapped) — not a tiled toplevel, so it is unaffected by window rules and
workspace switches and is never a party to the tiling layout. App windows are
ordinary xdg toplevels the tiler owns.

**Stacking.** Hyprland renders a fullscreen window **above the Top layer**
(only the Overlay layer stacks higher), and this model keeps every app window
fullscreen. The shell's main surface therefore lives on the **Overlay layer**
(`shell.qml`, `WlrLayershell.layer: WlrLayer.Overlay`): its `visible:` binding
already encodes "the shell should own or share the screen now" (home/idle, or a
drawer/QAM over an app), so a mapped shell must actually stack above the
fullscreen app — on the default Top layer, `returnToShell()` over a running
local app mapped the home screen *underneath* the app while stealing exclusive
keyboard focus (an invisible shell driving the D-pad), and the over-app drawers
could never display. When an app should own the screen the surface is unmapped,
so Overlay never covers a foregrounded app. The screenshot-flash and
launch-overlay windows use Overlay for the same reason. Every option below is verified against the
Hyprland source (file references inline); anything that did not survive that
check is listed under *Rejected*.

## Why the old reactive model structurally could not win

Everything ran on **one workspace**, and "one app fills the screen" was
maintained by *reacting* to Hyprland's event stream after the tiler had already
laid windows out, from **two independent enforcers**:

1. **Daemon** (`daemon/src/hyprland.rs`): `force_fullscreen` on `openwindow`;
   `enforce_active_fullscreen` (idempotent `fullscreen 0 set`) on
   `closewindow`/`movewindowv2`/`activewindowv2`.
2. **QML** (`AppLifecycleManager.qml`): on launch/resume, `focuswindow` then
   re-assert fullscreen — the resume address-path fired `hyprctl dispatch
   fullscreen 0`, a **non-idempotent toggle**, unconditionally.

The bad state (two app windows tiled side by side) was **representable** because
the single shared workspace lets the tiler split whenever two toplevels coexist
and neither is fullscreen. That gives three structural defects: two enforcers
with no serialization and conflicting semantics (idempotent `set` vs
non-idempotent toggle); a toggle is state-dependent while the layout is shared
and mutable; and once both windows are tiled, no declarative rule re-fullscreens
on focus — only the racy actor could.

**Today's incidents, mapped:**
1. *Split view on Steam resume (Plex backgrounded).* Resuming an already-
   fullscreen app ran `focuswindow` (Hyprland, via `on_focus_under_fullscreen=1`,
   correctly kept it fullscreen) → then QML's unconditional `fullscreen 0` toggle
   flipped it back **out** → both apps tiled → tiler split. Reproduces even with
   a healthy daemon; killing Plex "fixed" it because close-path enforcement then
   ran with a single window and no racing partner.
2. *Daemon deaf to a restarted Hyprland (>1h).* `resolve_hypr_signature()`
   trusted an inherited `HYPRLAND_INSTANCE_SIGNATURE` without a liveness check, so
   a long-lived `systemd --user` daemon stayed pinned to the dead instance after a
   render-hang kill+restart; reconnect backoff re-resolved the same stale
   signature forever. Silent — nothing surfaced it.
3. *Presenter flapped Shell↔Game* during the incident-1 focus churn — a symptom
   of reacting to a racing event stream.
4. *HDMI/CEC flap wedges Hyprland's render loop* (hyprctl still answers, frames
   frozen); kill+restart is the only recovery, which then triggers incident 2.

## The fix: declarative-first, single idempotent backstop, self-healing daemon

Remove the tiler's freedom to reach the bad state, collapse enforcement to one
idempotent authority, and make the daemon's compositor attachment self-heal.

**Declarative kiosk rules** (`config/hyprland.conf`, all verified against source):
- `windowrule = fullscreen on, match:class .+` — force every app window
  fullscreen at map (best-effort; gated on winning initial focus).
- `windowrule = suppress_event fullscreen maximize, match:class .+` — the kiosk
  owns fullscreen; ignore a window's own fullscreen/maximize *requests* so a game
  toggling fullscreen can't churn compositor state. `suppress_event` blocks only
  the window's requests, not `hyprctl dispatch`, so the daemon/keybinds keep
  control (`src/desktop/view/Window.cpp`).
- `misc:on_focus_under_fullscreen = 1` — focusing a window under a fullscreen one
  atomically un-fullscreens the old and fullscreens the new; the correct resume
  swap (`src/desktop/state/FocusState.cpp`).
- `misc:exit_window_retains_fullscreen = true` — closing a fullscreen app
  promotes the survivor to fullscreen natively; the declarative form of the
  daemon's old close-path enforcement (`src/config/values/ConfigValues.cpp`).

**Launch-time atomic placement** (`AppLifecycleManager.qml`): the app launch
dispatches `hyprctl dispatch exec [fullscreen] <cmd>`, so the app's first window
maps fullscreen from the start with nothing to correct post-hoc (exec-rule
syntax: `src/config/supplementary/executor/Executor.cpp`).

**Single enforcer** (`AppLifecycleManager.qml`): the QML `fullscreen 0` toggle
and its `hypr-active` read (`ensureFullscreen`/`ensureFullscreenQuery`) are
**removed**. `on_focus_under_fullscreen=1` + the daemon's idempotent `fullscreen
0 set` are now the only things that ever change fullscreen — the incident-1 root
cause is gone.

**Amendment (#347) — the resume path re-asserts fullscreen, idempotently.** The
statement above held only while every window mapped fullscreen. Prewarm (#238)
broke that premise: it launches apps with the `[silent]` exec rule, which maps
them **tiled** (`fs=0`), so the kiosk now deliberately carries a second,
non-fullscreen window from boot. Focusing a tiled window that sits *under* a
fullscreen one changes focus but not what is on screen — the resumed app is
focused-but-invisible, and when `on_focus_under_fullscreen` and the daemon's
`activewindowv2` backstop both miss, nothing else ever corrects it.

So `AppLifecycleManager` again asserts fullscreen after a resume — but with the
**idempotent `set` form** (`hyprctl dispatch fullscreen 0 set`), never the bare
toggle #308 removed. This is not a reversal of #308: the toggle *inverted* state,
so firing it at an already-fullscreen window flipped it back out (incident 1),
and whether it helped depended on who won the race. `set` *assigns* state, so it
is a no-op against a window that is already fullscreen and cannot invert
anything — two idempotent writers of the same state cannot race into a wrong
result the way a toggle and a setter could. It is the same form, for the same
reason, that `force_fullscreen` / `enforce_active_fullscreen` use in
`daemon/src/hyprland.rs`, and it targets the **active** window (no address
selector) exactly as `enforce_active_fullscreen` does.

**Focus landing is now verified, because an exit code cannot.** `hyprctl
dispatch` exits 0 even when its selector matched no window, so a focus that hit
nothing was structurally indistinguishable from one that worked. After a resume
dispatch the shell reads the daemon's `hypr-active` once (a single delayed read,
not a retry loop) and logs a `origin=resume-verify` trace line when the window
that became active is not the one it aimed at. Relatedly, the address-resolution
miss in `focusByAddress` no longer returns silently: it logs
(`origin=resume`) and falls back to a class-targeted focus, since an address
absent from the poll snapshot usually means the snapshot is stale rather than
that the app is gone. The decision + verification logic is pure and headlessly
tested in `shell/components/resumeFocus.js` (`tests/qml/tst_resumefocus.qml`).

Reconciling prewarm's `[silent]` mapping with the kiosk invariant — i.e. whether
prewarmed windows should map differently in the first place — is **deferred**
(#347 item 4).

**Amendment — the single-workspace premise is now asserted, not assumed.**
Everything above holds *"by construction on a single workspace"*, and until now
nothing enforced that clause. There is no `default_workspace`, no workspace
windowrule, no workspace keybind, and there was not one `dispatch workspace` or
`movetoworkspace` call anywhere in the shell or the daemon — so the premise was a
belief about the box, with no invariant check, no telemetry, and no recovery.

It was already false in the field. Observed on htpc-1 (2026-08-25): Plex HTPC on
workspace 1, Steam Big Picture on workspace 4, and the monitor **displaying
workspace 2, which held no windows at all**. Because the shell is a layer-shell
surface it draws regardless of workspace, so the home screen looked perfectly
healthy — but the instant a resume unmapped it, there was genuinely nothing
beneath to render and the TV went black. `dispatch focuswindow` could not rescue
it: it does not reliably follow across workspaces
([hyprwm/Hyprland#1611](https://github.com/hyprwm/Hyprland/issues/1611)), the same
issue cited under Phase 2 below. What put the windows there is **still unknown** —
which is precisely why the consolidation below logs every move rather than
self-healing silently.

The resume path therefore reads `hypr-monitors` for the displayed workspace and,
when its target has drifted elsewhere, dispatches
`movetoworkspace <displayed>,address:<addr>` *before* focusing
(`resumeFocus.resolveWorkspaceMove`, pure and headlessly tested in
`tests/qml/tst_resumefocus.qml`).

**Consolidate, not follow.** Switching the display to the window
(`dispatch workspace N`) would also resume correctly, but it accepts a
multi-workspace box permanently and keeps the active-but-empty workspace
reachable forever. Pulling the window onto the displayed workspace drains stray
workspaces back toward one every time the user resumes anything, which restores
the premise the rest of this document depends on. Every branch that cannot
establish *both* the target's workspace and the displayed one declines to move
and focuses anyway — degrading to exactly the pre-change behaviour, since a
`movetoworkspace` aimed at the wrong workspace would relocate a live window
off-screen, which is the failure being fixed.

**A verified miss now recovers.** `resume-verify` used to log and stop. But by
that point `appLaunched()` has already unmapped the shell, so a resume that
provably did not land leaves the TV showing whatever is underneath — in the
incident above, nothing. The shell now emits `resumeFailed` and returns to the
home screen (`resume-abandoned`). It is deliberately **not** `appClosed`: the app
is still running, and only the shell's belief that it came forward was wrong.

**Resumes are generationed, because a verified miss now ACTS.** A resume is a
chain of async hops — decide → read `hypr-monitors` → maybe `movetoworkspace` →
focus → settle → read `hypr-active` → judge — and every hop is a place a second
resume can start. All the state carrying one (`_pendingResumeDecision`,
`_pendingFocusDecision`, the move's `pending`, the shared verify timer) is
single-slot, and nothing recorded which resume owned which reply. Resuming two
apps in quick succession therefore let the first resume's verification judge
itself against compositor state the second had produced:

```
origin=resume-workspace address=0x…027310 workspace=2      (Steam)
origin=resume-workspace address=0x…bdc010 workspace=2      (Plex)
origin=resume-verify mode=address wanted=0x…027310 active=tv.plex.Plex
  reason=active-address-mismatch
origin=resume-abandoned mode=address wanted=0x…027310
```

Both consolidations were correct; only the bookkeeping crossed. This was latent
for as long as a miss merely logged — a crossed verification was a spurious
journal line nobody chased. Giving that branch a *consequence* is what promoted it
to a bug, which is why the fix ships in the same change.

`AppLifecycleManager._resumeGeneration` is bumped by every `focusByAddress` and
stamped onto the decision (`resumeFocus.stamp`); each hop drops its work once the
stamp is no longer current (`resumeFocus.isStale`). Suppression, not cancellation:
a superseded chain's dispatches were already issued and are harmless — the newer
resume's own dispatches land after them and win. What must not happen is a stale
chain reaching a *conclusion*. The drop is silent by design; a resume the user
replaced is not a fault, and logging it would train us to ignore real
`resume-verify` lines. An unstamped decision is treated as current, so a caller
that never opted into generations still works.

**Companion windows (Steam Remote Play).** Launching a game via Remote Play maps
the live video in a `streaming_client` toplevel while Big Picture stays mapped
behind it. Both get a drawer row, and that is correct — they are two different
destinations. What was wrong is the companion's **icon**: `streaming_client` has
no desktop entry, so the enumerator's class-name icon fallback resolves to nothing
and the row renders a blank letter-tile, unrecognisable as the game the user is
looking for. `appQuirks.identifyCompanionWindows` gives it the owner's icon
(falling back to the owner's class name when Big Picture is not mapped) and
changes nothing else — same rows, same order, same addresses and titles.

> **A collapse was tried first and reverted; do not reintroduce it casually.** An
> earlier revision merged the pair into a single row. It was wrong twice over.
> First, it removed the user's only route to the live stream window — confirmed by
> the reporter, who went looking for the stream in the drawer and found only a row
> that led to Big Picture. Second, its justification was a
> `resume-verify … active-address-mismatch` line read as "this row targets the
> wrong window" — but that is **also** the signature of the crossed-verification
> race that `_resumeGeneration` fixes above. The evidence was ambiguous between
> "mistargeted row" and "raced verification", and the race is the better-supported
> explanation. `tests/qml/tst_appquirks.qml` pins the window set as a regression
> guard against re-collapsing.

**Self-healing daemon** (`daemon/src/session_env.rs`, `hyprland.rs`): signature
resolution scans `$XDG_RUNTIME_DIR/hypr/` for the live socket dir *before*
trusting an inherited env var, so a reconnect re-attaches to a restarted Hyprland
(kills incident 2). Connect-time logging names the attached instance; five
consecutive failed reconnects escalate to a loud `error!` naming the deaf-daemon
condition.

Together these make the contract hold **by construction on a single workspace**:
every window is fullscreen at map and can't un-fullscreen itself; focus swaps are
atomic; closes promote the survivor; and the one actor that broke it is gone. No
frame ever shows two tiled app windows.

## Evaluation of the proposed lockdown ideas

| Idea | Verdict | Notes |
|---|---|---|
| Launch-time atomic `[fullscreen]` placement | **Adopted** | Highest-leverage; app is fullscreen at map. Launching is QML (`hyprctl dispatch exec`), not the daemon — `intent app:` just routes to QML. |
| `suppress_event fullscreen maximize` | **Adopted** | Effect name is `suppress_event` (underscored) in 0.55.2's `WindowRuleEffectContainer.cpp` EFFECT_STRINGS; the legacy `windowrulev2` spelling `suppressevent` fails config parse on-device. Tokens (`fullscreen`/`maximize`) verified in `Window.cpp`. `activate` deliberately **not** suppressed — `focus_on_activate`/launch-focus rely on it; focus-steal is prevented structurally by the fullscreen invariant instead. |
| `new_window_takes_over_fullscreen = 2` | **Rejected** | **Does not exist** in Hyprland 0.55 (`ConfigValues.cpp` has no such key) — setting it errors in `hyprctl configerrors`. Its intent is covered by the fullscreen windowrule + `on_focus_under_fullscreen`; adopted `exit_window_retains_fullscreen` (a real option) instead. |
| Dynamic per-class `windowrulev2` from the daemon | **Rejected (obviated)** | The wildcard `match:class .+` already applies to every class; runtime per-class registration adds churn for no coverage gain. |
| Strip default Hyprland binds | **N/A (already satisfied)** | Hyprland ships **no** default keybinds; the kiosk config already declares only the super-intent set + `SUPER,Q`. Nothing to strip. |
| `idleinhibit fullscreen` windowrule | **Rejected** | Every app is always fullscreen, so this would inhibit idle for *any* running app and defeat the shell's configurable sleep timer (Power page). Media players already send the Wayland idle-inhibit protocol when actually playing — the nuance a blanket rule loses. |
| Compositor watchdog in the daemon | **Partial (Phase 1) / Phase 2** | The event-socket-dead case (incident 2) is now detected + escalated. The render-wedge (incident 4 — frozen frames while `hyprctl` still answers) is **not** IPC-observable from the daemon; detecting it needs a render-side heartbeat (Phase 2). Auto-heal (kill Hyprland → restart plasmalogin → restart daemon) is Phase 2. |
| "Running apps" list in the NavigationDrawer | **Phase 2 (UI, not implemented)** | The daemon window model already exposes everything it needs — each running window's `address`, `class`, `workspace`, `focusHistoryId` via `hypr-clients`. So `A` → `focusByAddress` (today) or `dispatch workspace N` (under Phase-2 isolation) is a race-free switch. Phase 1 does not paint it into a corner. |

**Shell-side selective idle-inhibitor (#195, implemented).** Instead of the
rejected blanket `idleinhibit fullscreen` windowrule, the shell asserts a Wayland
idle-inhibitor only when it *knows* video is playing: its own `streaming` state,
or an `appRunning` app while an MPRIS player reports Playing (Plex/mpv) —
`IdleInhibitController` computes the policy; a dedicated per-screen `IdleInhibitor`
window in `shell.qml` asserts it. That window is **Background-layer + mapped only
while inhibiting** so it sits below the fullscreen app and preserves Hyprland
direct scanout (an Overlay-layer surface would force compositing). Static app
screens and the idle home screen are deliberately left un-inhibited so a
compositor-level idle daemon (hypridle/DPMS — a system concern outside this repo,
which honors these inhibitors) can still blank them for OLED burn-in protection.

## Interaction with the Steam widget (PR #306)

#306's resume path (`SteamCard`/`MoonlightWidget` → `focusByAddress` →
`appLaunched`) is **unchanged in shape** and **improves** under this work: the
`fullscreen 0` toggle that `focusByAddress` used to trigger is gone, so resuming
Steam while Plex is backgrounded swaps fullscreen atomically instead of splitting.
No API change — `focusByAddress(address)` still focuses the window and emits
`appLaunched`. **Under a future Phase 2 (per-app workspaces)** resume becomes
`dispatch workspace N` (because `dispatch focuswindow` does not reliably follow
across workspaces — [hyprwm/Hyprland#1611](https://github.com/hyprwm/Hyprland/issues/1611));
#306 would then read the target window's `workspace` (already in the
`hypr-clients` model) rather than call `focuswindow`. Nothing in #306 needs to
change for the current phase.

## Phase 2 (deferred — the strongest form, needs on-device iteration)

> **Reconciling Phase 2 with the consolidation amendment above.** The two want
> opposite things and cannot both be live: consolidation drains every app window
> onto the displayed workspace, while isolation deliberately keeps them apart.
> Consolidation is the *single-workspace* model finally enforcing its own premise,
> and it must be **removed**, not merely bypassed, if isolation is ever adopted —
> at which point resume becomes `dispatch workspace N` as described below. The
> pure decision already lives behind one function
> (`resumeFocus.resolveWorkspaceMove`), so the swap is contained to it and its
> tests.

**Per-app-workspace isolation.** Assign each app window its own workspace
(class-grouped, so Steam's splash + main share one) so two app windows can never
occupy the same workspace — the split state becomes *unrepresentable* rather than
merely *prevented*. Launch: `exec [workspace N silent; fullscreen]`. Resume:
`dispatch workspace N` (not `focuswindow`, per #1611). This is a bigger change
(workspace allocation, resume-path rework, Steam multi-window grouping) that
needs on-device iteration, which is why it is deferred rather than shipped blind.
The single-workspace model above already satisfies the contract; isolation is
strictly-stronger insurance. Also Phase 2: the render-wedge heartbeat + auto-heal
watchdog, and the NavigationDrawer running-apps list.

## On-device validation checklist (before merge)

Deploy to htpc-1 and confirm:
- [ ] **Two apps backgrounded, switch between them, never a split view.** Launch
  Plex HTPC, launch Steam (Plex backgrounds), resume Plex from a home card, resume
  Steam — each switch shows exactly one fullscreen app, never a side-by-side tile.
- [ ] **App-initiated fullscreen churn is absorbed.** In a game/player, toggle its
  own fullscreen/menu repeatedly — the kiosk stays fullscreen (suppress_event).
- [ ] **Close promotes the survivor.** With two apps running, quit the foreground
  one — the backgrounded app comes forward fullscreen, no split
  (`exit_window_retains_fullscreen`).
- [ ] **Fresh launch is fullscreen immediately** — no visible tiled flash before
  fullscreen (the `[fullscreen]` exec-rule).
- [ ] **Kiosk survives a compositor restart.** Kill + restart Hyprland; confirm the
  daemon re-attaches (journal: `event listener attached to …`), fullscreen
  enforcement + presenter follow-focus resume, and no manual daemon restart is
  needed.
- [ ] **Config parses clean:** `hyprctl configerrors` is empty after reload.
- [ ] Single-app launch/resume still fullscreens correctly with the daemon as the
  only enforcer (QML toggle removed).

## Diagnosing "who launched this window?"

The kiosk contract is violated whenever *two* windows of one app map, or a window
maps unplaced. Both failure modes look identical in the compositor — every launch
the shell issues arrives as a `hyprctl dispatch exec` child of Hyprland, so the
compositor cannot say which of the shell's several launch paths issued it.

The shell therefore traces every app-launch shell-out through one choke point
(`shell/components/AppLifecycleManager.qml`'s `_dispatchExec`, formatted by
`shell/components/launchTrace.js`). One boot's journal answers the question:

```bash
journalctl --user -b -t tv-shell-quickshell | grep 'tv-shell:launch'
```

Each line carries the call path that issued the launch and the window rule it
used:

```
[tv-shell:launch] origin=launch rule=[fullscreen] app=Plex class=tv.plex.Plex comm=plex exec=/usr/bin/Plex
```

| `origin` | Call path | Rule |
|---|---|---|
| `launch` | `launchDesktopApp` — foreground launch | `[fullscreen]` |
| `prewarm` | `prewarmApp` — silent login prewarm | `[silent]` |
| `redeliver` | `redeliverAndFocus` — single-instance exec redelivery | `none` |
| `stream` | `StreamManager._launchMoonlight` — direct child, not via `hyprctl` | `none` |
| `prewarm-decision` | the login prewarm pass's one evaluation (what it saw, what it chose) | — |
| `resume` | `focusByAddress` — the address missed the window snapshot: either a class fallback (`mode=class`) or nothing actionable (`mode=none`) | — |
| `resume-workspace` | the resume target was on a different workspace than the display and was consolidated onto it — `address=` moved, `workspace=` destination | — |
| `resume-verify` | a resume focus dispatch that did NOT land — `wanted=` vs `active=` names the miss | — |
| `resume-abandoned` | that miss was recovered by returning to the shell rather than leaving the TV on whatever was underneath | — |

The two `resume*` origins carry no `rule=`/`comm=` (they focus an existing
window rather than exec a new one). They are logged **only on a fault** — a
resume that resolves and lands adds no line, so any `origin=resume*` line in the
journal is itself the finding.

`comm=` is the process name `ps -eo comm=` reports, so a journal line correlates
directly with a live pid. **Two lines with different `origin` values for the same
`comm` is a double launch, and the `origin` names the second culprit.** A launch
logging `rule=none` will not be placed fullscreen at map time — it depends on the
`windowrule = fullscreen` backstop and the daemon's `openwindow` enforcement
instead.
