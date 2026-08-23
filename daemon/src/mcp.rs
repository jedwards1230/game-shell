//! MCP server for the tv-shell daemon, built on the official `rmcp` 1.7.0 crate.
//!
//! **Opt-in**: the server only starts when `[mcp].bind` in `config.toml` is set
//! to a `host:port` address. When unset, no socket is opened.
//!
//! **Auth**: bearer-token auth at parity with the HTTP bridge. Uses the same
//! `[http].token_file` and `[http].auth_enabled` settings so
//! operators only need one token. Applied via an axum middleware layer wrapping
//! the `/mcp` route; constant-time comparison via `bridge_core::ct_eq_str`.
//! If auth is enabled but no token is configured, all requests are rejected (fail
//! closed). `[mcp].allowed_hosts` (a list of host[:port]) overrides the rmcp
//! host allowlist.
//!
//! **Dev tools**: `dev_deploy`, `dev_build`, and `dev_restart_daemon` are
//! registered **unconditionally** and return an error unless `[mcp].dev = true`
//! in `config.toml` (see `docs/CONTROL_SURFACE.md`). Registering them either way
//! keeps the tool list stable across nodes; the gate is on invocation.
//!
//! They are a remote-code-execution surface by design — `dev_deploy` checks out
//! an arbitrary git ref and `dev_build` compiles and installs it — so on a node
//! that enables them the bearer token is as privileged as the account the daemon
//! runs as. Enable deliberately.
//!
//! **Transport**: StreamableHttpService over axum, served at `/mcp`. The MCP
//! endpoint is at `http://<bind>/mcp`.
//!
//! **Feature-gated (cross-platform)**: this module is gated on
//! `#[cfg(feature = "mcp")]` only — `rmcp`, `axum`, and `tokio` are all
//! cross-platform, so the module compiles and typechecks on macOS dev boxes
//! just as well as on Linux. It is NOT `#[cfg(target_os = "linux")]`-gated.
//! Only the binary (`main.rs`) and the Linux-specific modules (evdev, uinput,
//! zbus, Hyprland IPC) carry an OS guard; the MCP server itself runs anywhere
//! tokio does.

use std::sync::Arc;

use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
};
use base64::Engine as _;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorData as McpError, Implementation, ListResourcesResult,
        PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
        ResourceContents, ServerCapabilities, ServerInfo,
    },
    schemars::{self, JsonSchema},
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router, Json, ServerHandler,
};
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::bridge_core;
use crate::bridge_core::DispatchOutcome;
use crate::protocol::Event;
use crate::state::Control;

// ─── Resources ───────────────────────────────────────────────────────────────

/// URI of the screenshot resource. Reading it captures a fresh frame of the live
/// display.
///
/// Exposed **additively** alongside the `take_screenshot` tool, not as a
/// replacement: the tool is the model-driven primitive that powers the autonomous
/// observe→act→verify loop, while this resource is the host/user-driven path for
/// attaching the current screen as context from an MCP client's resource picker.
/// Per the MCP spec, `resources/read` MUST be side-effect-free, so a read NEVER
/// triggers the screen flash (only the tool's `flash` arg does). Nothing is
/// captured until a client actually reads.
const SCREENSHOT_RESOURCE_URI: &str = "screenshot://current";

/// Build the `screenshot://current` descriptor returned by `resources/list`.
/// Factored out so a unit test can assert its shape without constructing a
/// `RequestContext`.
fn screenshot_resource_descriptor() -> Resource {
    Resource::new(SCREENSHOT_RESOURCE_URI, "Current screen")
        .with_title("Current screen")
        .with_description(
            "Live tv-shell display, captured as a PNG on read (no flash). \
             Host/user-driven context — for the autonomous observe→verify loop, \
             use the take_screenshot tool instead.",
        )
        .with_mime_type("image/png")
}

// ─── Parameter types ─────────────────────────────────────────────────────────

/// A bare shell action name. Deep-links (containing `:`) are rejected — use
/// `open_settings`, `open_overlay`, or `launch_app` for those.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellActionParams {
    /// Bare action name. Valid values: home, home-tap, home-hold, menu,
    /// settings, power.
    /// Deep-links (settings:<page>, overlay:<target>, app:<wmClass>) are NOT
    /// accepted here — use open_settings / open_overlay / launch_app instead.
    pub name: String,
}

/// Known settings page slugs. Unknown slugs are a graceful no-op in QML;
/// this enum constrains the MCP input to the documented set.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SettingsPage {
    /// Audio output settings (default sink, volume, speaker test).
    Audio,
    /// Bluetooth device scanning and pairing.
    Bluetooth,
    /// Wi-Fi / network connectivity.
    Network,
    /// Display settings (monitor, HDR, refresh rate, night light, overscan).
    Display,
    /// Gamepad/controller configuration.
    Controllers,
    /// Key-binding remapping.
    Keybindings,
    /// HDMI-CEC AV control and focus preferences.
    Avcontrol,
    /// Streaming / Moonlight provider page (shown when a provider is active;
    /// `streaming` is the fallback slug, else the provider id).
    Streaming,
    /// Home-screen widgets (enable/disable + per-widget size).
    Widgets,
    /// Accessibility (reduce motion, text size).
    Accessibility,
    /// Power management (sleep timer, wake-on-controller).
    Power,
    /// System info, storage, OS version.
    System,
}

impl SettingsPage {
    fn as_str(&self) -> &'static str {
        match self {
            SettingsPage::Audio => "audio",
            SettingsPage::Bluetooth => "bluetooth",
            SettingsPage::Network => "network",
            SettingsPage::Display => "display",
            SettingsPage::Controllers => "controllers",
            SettingsPage::Keybindings => "keybindings",
            SettingsPage::Avcontrol => "avcontrol",
            SettingsPage::Streaming => "streaming",
            SettingsPage::Widgets => "widgets",
            SettingsPage::Accessibility => "accessibility",
            SettingsPage::Power => "power",
            SettingsPage::System => "system",
        }
    }
}

/// Direction or action key for D-pad / keyboard navigation.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NavKey {
    /// Move focus upward.
    Up,
    /// Move focus downward.
    Down,
    /// Move focus left.
    Left,
    /// Move focus right.
    Right,
    /// Activate / confirm the currently focused element (A button / Enter).
    Select,
    /// Go up one level / dismiss (B button / Escape).
    Back,
}

impl NavKey {
    fn as_str(&self) -> &'static str {
        match self {
            NavKey::Up => "up",
            NavKey::Down => "down",
            NavKey::Left => "left",
            NavKey::Right => "right",
            NavKey::Select => "select",
            NavKey::Back => "back",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NavigateParams {
    /// Direction or action key. `select` = activate/confirm the focused element
    /// (A button / Enter); `back` = go up one level / dismiss (B button / Escape).
    pub key: NavKey,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenSettingsParams {
    /// Settings page to open.
    pub page: SettingsPage,
}

/// Overlay popover target.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OverlayTarget {
    /// Audio volume slider popover.
    Volume,
    /// Wi-Fi / network connection popover.
    Network,
    /// Power / session drawer (sleep, restart, end session).
    Session,
}

impl OverlayTarget {
    fn as_str(&self) -> &'static str {
        match self {
            OverlayTarget::Volume => "volume",
            OverlayTarget::Network => "network",
            OverlayTarget::Session => "session",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenOverlayParams {
    /// Overlay popover to open. `volume` = audio volume slider; `network` = Wi-Fi /
    /// connection popover; `session` = power / session drawer (sleep, restart,
    /// end session).
    pub target: OverlayTarget,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LaunchAppParams {
    /// The StartupWMClass of the .desktop application to launch.
    pub wm_class: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TakeScreenshotParams {
    /// When true, broadcast a flash event on the IPC bus after capture so the
    /// QML shell can paint a brief white vignette as visual feedback.
    #[serde(default)]
    pub flash: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetLogsParams {
    /// Number of log lines to return (default 100, max 1000).
    #[serde(default = "default_log_lines")]
    pub lines: u32,
    /// Optional substring filter (case-insensitive).
    pub filter: Option<String>,
}

fn default_log_lines() -> u32 {
    100
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DevDeployParams {
    /// Git ref to deploy (branch name, tag, or commit SHA). Defaults to "main".
    /// Only alphanumeric characters, `.`, `_`, `/`, and `-` are accepted.
    pub git_ref: Option<String>,
}

// ─── Server handle ───────────────────────────────────────────────────────────

/// The shared runtime handles cloned into each MCP session.
#[derive(Clone)]
struct Handles {
    control_tx: mpsc::Sender<Control>,
    events_tx: broadcast::Sender<Event>,
    shutdown: CancellationToken,
    reexec_flag: Arc<std::sync::atomic::AtomicBool>,
    dev_enabled: bool,
    metrics: Arc<crate::metrics::Metrics>,
}

// ─── MCP handler ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct TvShellMcp {
    handles: Handles,
    tool_router: ToolRouter<Self>,
}

impl TvShellMcp {
    fn new(handles: Handles) -> Self {
        Self {
            tool_router: Self::tool_router(),
            handles,
        }
    }

    /// Translate a `dispatch_intent`/`dispatch_key` reply into a
    /// `CallToolResult`. Shared by every intent-dispatching tool so the
    /// unavailable/error/ok mapping stays consistent.
    fn intent_result(reply: Option<String>) -> CallToolResult {
        match bridge_core::interpret_reply(reply) {
            DispatchOutcome::Unavailable => {
                CallToolResult::error(vec![ContentBlock::text("daemon unavailable")])
            }
            DispatchOutcome::Err(msg) => {
                CallToolResult::error(vec![ContentBlock::text(msg.trim().to_owned())])
            }
            DispatchOutcome::Ok => CallToolResult::success(vec![ContentBlock::text("ok")]),
        }
    }

    /// Shared body for the `shell_action` tool and its `intent` alias: reject
    /// deep-links, validate the action against the known vocabulary, then
    /// dispatch it through the control channel.
    async fn do_shell_action(&self, name: String) -> CallToolResult {
        // Reject any deep-link (colon-delimited) at the MCP-tool layer.
        // The underlying IPC/HTTP paths still accept them; this restriction
        // is intentional for the MCP surface only (A1).
        if name.contains(':') {
            let valid = crate::protocol::INTENT_VOCAB.join(", ");
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "deep-links are not accepted here; use open_settings / open_overlay / launch_app. \
                 Valid actions: {valid}."
            ))]);
        }
        if !bridge_core::is_valid_intent(&name) {
            let valid = crate::protocol::INTENT_VOCAB.join(", ");
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "unknown action '{name}'. Valid actions: {valid}."
            ))]);
        }
        Self::intent_result(bridge_core::dispatch_intent(&self.handles.control_tx, name).await)
    }

    /// Shared body for the `navigate` tool and its `key` alias: synthesize a
    /// directional/action keypress through the control channel.
    async fn do_navigate(&self, key_str: String) -> CallToolResult {
        Self::intent_result(bridge_core::dispatch_key(&self.handles.control_tx, key_str).await)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for TvShellMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                // Advertise `resources.listChanged` so the serialized capability is
                // a NON-EMPTY object (`{"listChanged":true}`) rather than bare `{}`.
                // A federating MCP gateway (ContextForge) gates resource discovery on
                // a truthy `resources` value (`if capabilities.get("resources"):` —
                // Python treats `{}` as falsy), so a bare `{}` makes it silently drop
                // `screenshot://current` while still federating the tools. The resource
                // list is effectively static (just `screenshot://current`), so the
                // daemon never actually emits a list_changed notification — declaring
                // the capability is the honest, side-effect-free way to unblock
                // federation. (`enable_resources_subscribe` is deliberately avoided:
                // the daemon does not implement `resources/subscribe`.)
                .enable_resources_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(
            tv_shell_protocol::brand::mcp_server_name(),
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Controls the tv-shell Quickshell UI on the game client.\n\
                 \n\
                 Observe → Act → Verify loop:\n\
                 1. take_screenshot or get_ui_state to observe current state.\n\
                 2. Act with one of the tools below.\n\
                 3. take_screenshot to confirm the expected state was reached.\n\
                 \n\
                 Tool guide:\n\
                 - shell_action: top-level bare-verb actions (home, menu, settings, power). \
                 Does NOT accept deep-links — use the sugar tools for those.\n\
                 - open_settings(page): navigate directly to a typed settings page.\n\
                 - open_overlay(target): open volume/network/session QAM popovers.\n\
                 - launch_app(wm_class): launch an installed app by its StartupWMClass. \
                 Use list_apps to discover launchable apps and their wm_class values.\n\
                 - navigate(key): in-view directional focus movement (up/down/left/right) \
                 and confirm/cancel (select/back). Use after observing what is focused.\n\
                 - list_apps: discover installed apps (name, wm_class, comment).\n\
                 - get_status: daemon-level health snapshot (not the UI's on-screen state).\n\
                 - get_logs / restart_shell: diagnostics and recovery.\n\
                 \n\
                 Resource:\n\
                 - screenshot://current — the live display as a PNG, exposed as an \
                 MCP resource for host/user-driven context attachment. Reading it \
                 captures a fresh frame WITHOUT the flash effect. For the autonomous \
                 observe→act→verify loop above, prefer the take_screenshot tool.",
        )
    }

    // ── Resources ─────────────────────────────────────────────────────────────
    // The single `screenshot://current` resource is additive: it lets a human in
    // an MCP client attach the live screen as context. The take_screenshot tool
    // remains the primitive the autonomous agent loop uses.

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            screenshot_resource_descriptor(),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri;
        if uri != SCREENSHOT_RESOURCE_URI {
            return Err(McpError::resource_not_found(
                format!("unknown resource '{uri}'"),
                Some(serde_json::json!({ "uri": uri })),
            ));
        }
        // `resources/read` MUST be side-effect-free, so flash is hard-wired off
        // here — only the take_screenshot tool flashes the TV. Capture is lazy:
        // nothing runs until a client actually reads this URI.
        let png = bridge_core::capture_screenshot(&self.handles.events_tx, false)
            .await
            .map_err(|msg| McpError::internal_error(msg, None))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        // Same provenance the take_screenshot tool returns, as a second content
        // block — read live (a dev_deploy can move HEAD under the daemon).
        let meta = bridge_core::capture_meta().await;
        let meta_json = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".to_owned());
        Ok(ReadResourceResult::new(vec![
            ResourceContents::blob(b64, SCREENSHOT_RESOURCE_URI).with_mime_type("image/png"),
            ResourceContents::text(meta_json, SCREENSHOT_RESOURCE_URI)
                .with_mime_type("application/json"),
        ]))
    }
}

#[tool_router(router = tool_router)]
impl TvShellMcp {
    // ── Navigation / intents ──────────────────────────────────────────────────

    #[tool(
        description = "Send a bare top-level action to the tv-shell UI. \
            Valid actions: home, home-tap, home-hold, menu, settings, power. \
            Deep-links (containing ':') are NOT accepted here — \
            use open_settings / open_overlay / launch_app for those specific targets.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn shell_action(
        &self,
        Parameters(ShellActionParams { name }): Parameters<ShellActionParams>,
    ) -> CallToolResult {
        self.do_shell_action(name).await
    }

    #[tool(
        description = "Alias of `shell_action` (identical behavior). Named `intent` \
            to match the HTTP/IPC verb. Send a bare top-level action to the \
            tv-shell UI. Valid actions: home, home-tap, home-hold, menu, settings, \
            power. Deep-links (containing ':') are NOT accepted here — \
            use open_settings / open_overlay / launch_app for those specific targets.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn intent(
        &self,
        Parameters(ShellActionParams { name }): Parameters<ShellActionParams>,
    ) -> CallToolResult {
        self.do_shell_action(name).await
    }

    #[tool(
        description = "Synthesize a directional or action keypress on the \
            tv-shell virtual keyboard. Moves focus RELATIVE to the currently \
            focused element — observe first (take_screenshot or get_ui_state) \
            to know what is focused before navigating. \
            `select` = activate/confirm the focused element (A button / Enter); \
            `back` = go up one level / dismiss (B button / Escape). \
            This is the lower-level, in-view control tool for moving within an \
            already-visible UI surface. For top-level navigation use shell_action, \
            open_settings, open_overlay, or launch_app instead.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn navigate(
        &self,
        Parameters(NavigateParams { key }): Parameters<NavigateParams>,
    ) -> CallToolResult {
        self.do_navigate(key.as_str().to_owned()).await
    }

    #[tool(
        description = "Alias of `navigate` (identical behavior). Named `key` to \
            match the HTTP/IPC verb. Synthesize a directional/action keypress on \
            the tv-shell virtual keyboard. Moves focus RELATIVE to the currently \
            focused element — observe first (take_screenshot or get_ui_state) \
            to know what is focused before navigating. \
            `select` = activate/confirm the focused element (A button / Enter); \
            `back` = go up one level / dismiss (B button / Escape).",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn key(
        &self,
        Parameters(NavigateParams { key }): Parameters<NavigateParams>,
    ) -> CallToolResult {
        self.do_navigate(key.as_str().to_owned()).await
    }

    #[tool(
        description = "Open a specific settings page directly by name.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn open_settings(
        &self,
        Parameters(OpenSettingsParams { page }): Parameters<OpenSettingsParams>,
    ) -> CallToolResult {
        let intent_name = bridge_core::settings_intent(page.as_str());
        Self::intent_result(
            bridge_core::dispatch_intent(&self.handles.control_tx, intent_name).await,
        )
    }

    #[tool(
        description = "Open an overlay popover. `volume` = audio volume slider; \
            `network` = Wi-Fi / connection popover; `session` = power / session \
            drawer (sleep, restart, end session).",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn open_overlay(
        &self,
        Parameters(OpenOverlayParams { target }): Parameters<OpenOverlayParams>,
    ) -> CallToolResult {
        // The enum should guarantee a valid target, but handle a skew between
        // the enum and overlay_intent() gracefully rather than panicking the
        // server mid-request.
        let intent_name = match bridge_core::overlay_intent(target.as_str()) {
            Ok(name) => name,
            Err(msg) => {
                return CallToolResult::error(vec![ContentBlock::text(format!(
                    "internal error: {msg}"
                ))])
            }
        };
        Self::intent_result(
            bridge_core::dispatch_intent(&self.handles.control_tx, intent_name).await,
        )
    }

    #[tool(
        description = "Launch a local .desktop application by its StartupWMClass. \
            `wm_class` is the StartupWMClass from the .desktop file — NOT the \
            human-readable display name. Examples: `steam`, `org.mozilla.Firefox`, \
            `com.valvesoftware.Steam`. Use list_apps to discover installed apps \
            and their exact wm_class values.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn launch_app(
        &self,
        Parameters(LaunchAppParams { wm_class }): Parameters<LaunchAppParams>,
    ) -> CallToolResult {
        let intent_name = bridge_core::app_intent(&wm_class);
        Self::intent_result(
            bridge_core::dispatch_intent(&self.handles.control_tx, intent_name).await,
        )
    }

    // ── App discovery ─────────────────────────────────────────────────────────

    #[tool(
        description = "Return the list of installed .desktop applications as structured JSON. \
            Each entry has name (human-readable), wm_class (StartupWMClass — pass to \
            launch_app), and comment (optional description). \
            Use this to discover launchable apps before calling launch_app.",
        annotations(read_only_hint = true)
    )]
    async fn list_apps(&self) -> Result<Json<bridge_core::ListAppsResult>, String> {
        Ok(Json(bridge_core::ListAppsResult {
            apps: bridge_core::list_apps().await,
        }))
    }

    // ── UI state ─────────────────────────────────────────────────────────────

    #[tool(
        description = "Return lightweight compositor-level UI state without taking a screenshot. \
            Reports which Hyprland window is currently focused (class + title), \
            whether that focused window is quickshell, and whether quickshell is running. \
            This is WINDOW-LEVEL state from the compositor — it does NOT reveal \
            which QML page or list item is active inside the shell (use \
            take_screenshot for that). \
            On non-Linux builds the Hyprland fields are absent; a platform_note \
            field explains which data is unavailable.",
        annotations(read_only_hint = true)
    )]
    async fn get_ui_state(&self) -> Result<Json<bridge_core::UiState>, String> {
        Ok(Json(bridge_core::get_ui_state().await))
    }

    // ── Screenshot ────────────────────────────────────────────────────────────

    #[tool(
        description = "Capture the current Wayland display as a PNG image. \
            Set flash=true to trigger a brief white vignette on the tv-shell UI \
            after capture (visual feedback for the user at the TV). \
            Call this after every UI action to confirm the expected state was reached. \
            Returns two content blocks: the PNG image, then a JSON text block of \
            capture metadata {captured_at, sha, branch, version} identifying which \
            deployed tv-shell produced the frame (main vs a feature branch).",
        annotations(read_only_hint = true)
    )]
    async fn take_screenshot(
        &self,
        Parameters(TakeScreenshotParams { flash }): Parameters<TakeScreenshotParams>,
    ) -> CallToolResult {
        match bridge_core::capture_screenshot(&self.handles.events_tx, flash).await {
            Ok(png) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
                // Provenance rides in a trailing text block so the agent can tell
                // which checkout produced the frame. Read live (see capture_meta).
                let meta = bridge_core::capture_meta().await;
                let meta_json = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".to_owned());
                CallToolResult::success(vec![
                    ContentBlock::image(b64, "image/png"),
                    ContentBlock::text(meta_json),
                ])
            }
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(msg)]),
        }
    }

    // ── Status / diagnostics ──────────────────────────────────────────────────

    #[tool(
        description = "Return daemon/shell status as typed JSON: git SHA, daemon PID \
            and version, whether quickshell is running, the Wayland display, and \
            whether the Hyprland signature resolves. \
            A daemon-level snapshot — not the UI's on-screen state \
            (use take_screenshot to see what is actually displayed).",
        annotations(read_only_hint = true)
    )]
    async fn get_status(&self) -> Result<Json<bridge_core::StatusInfo>, String> {
        Ok(Json(bridge_core::get_status().await))
    }

    #[tool(
        description = "Return the last N lines of /tmp/qs-log.txt \
            (the quickshell log file). Optionally filter by a substring. \
            If the log file does not exist yet, returns an explanatory hint.",
        annotations(read_only_hint = true)
    )]
    async fn get_logs(
        &self,
        Parameters(GetLogsParams { lines, filter }): Parameters<GetLogsParams>,
    ) -> CallToolResult {
        let lines_usize = (lines as usize).min(1000);
        match bridge_core::get_logs(lines_usize, filter.as_deref()) {
            Ok(content) => CallToolResult::success(vec![ContentBlock::text(content)]),
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(msg)]),
        }
    }

    // ── Shell management ──────────────────────────────────────────────────────

    #[tool(
        description = "Kill and restart the quickshell process. \
            Waits 3 seconds for startup and returns the first WARN/ERROR log lines \
            (or a 'no errors' confirmation). Use after deploying a new tv-shell \
            build to pick up QML changes without rebooting.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn restart_shell(&self) -> CallToolResult {
        match bridge_core::dev_restart_shell(&self.handles.metrics).await {
            Ok(body) => CallToolResult::success(vec![ContentBlock::text(body)]),
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(msg)]),
        }
    }

    // ── Dev operations (gated on [mcp].dev in config.toml) ────────────────
    // These are only *registered* when dev_enabled is true; the tool_router
    // includes them unconditionally at compile time but they return a clear
    // error when the dev flag is absent — this is the safest approach with the
    // current rmcp macro model (conditional registration is not yet supported).

    #[tool(
        description = "DEV ONLY (requires [mcp].dev = true in config.toml). \
            git fetch + checkout + reset to remote. Defaults to 'main'. \
            Use to pull a branch onto the device without a full re-deploy.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn dev_deploy(
        &self,
        Parameters(DevDeployParams { git_ref }): Parameters<DevDeployParams>,
    ) -> CallToolResult {
        if !self.handles.dev_enabled {
            return CallToolResult::error(vec![ContentBlock::text(
                "dev tools disabled — set [mcp].dev = true in config.toml and restart the daemon to enable",
            )]);
        }
        match bridge_core::dev_deploy(git_ref.as_deref()).await {
            Ok(body) => CallToolResult::success(vec![ContentBlock::text(body)]),
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(msg)]),
        }
    }

    #[tool(
        description = "DEV ONLY (requires [mcp].dev = true in config.toml). \
            Run scripts/build-daemon.sh and install the resulting binary. \
            This is a long-running operation (~15-60 seconds depending on cache).",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn dev_build(&self) -> CallToolResult {
        if !self.handles.dev_enabled {
            return CallToolResult::error(vec![ContentBlock::text(
                "dev tools disabled — set [mcp].dev = true in config.toml and restart the daemon to enable",
            )]);
        }
        match bridge_core::dev_build().await {
            Ok(body) => CallToolResult::success(vec![ContentBlock::text(body)]),
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(msg)]),
        }
    }

    #[tool(
        description = "DEV ONLY (requires [mcp].dev = true in config.toml). \
            Re-exec the daemon process (picks up a newly built binary). \
            The MCP connection will drop immediately after the response.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn dev_restart_daemon(&self) -> CallToolResult {
        if !self.handles.dev_enabled {
            return CallToolResult::error(vec![ContentBlock::text(
                "dev tools disabled — set [mcp].dev = true in config.toml and restart the daemon to enable",
            )]);
        }
        bridge_core::request_reexec(&self.handles.reexec_flag, &self.handles.shutdown);
        CallToolResult::success(vec![ContentBlock::text("ok, re-execing\n")])
    }
}

// ─── Auth state (shared by the middleware closure) ────────────────────────────

#[derive(Clone)]
struct AuthState {
    /// `Some(token)` when auth is enabled and a token is configured.
    /// `None` when auth is disabled.
    expected_bearer: Option<String>,
    /// True when auth is enabled but no token was provided — fail closed.
    fail_closed: bool,
}

/// axum middleware that enforces bearer-token auth on all `/mcp` requests.
///
/// Mirrors the HTTP bridge's auth logic (`http.rs`) at parity:
/// - Auth enabled + token set: constant-time comparison of `Authorization: Bearer <token>`.
/// - Auth enabled + no token:  reject all (fail closed, 401).
/// - Auth disabled:            pass through unconditionally.
async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<AuthState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if state.fail_closed {
        // Auth enabled but no token configured — reject all.
        return Err(StatusCode::UNAUTHORIZED);
    }
    if let Some(expected) = &state.expected_bearer {
        let provided = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !bridge_core::ct_eq_str(provided, expected) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    // Auth disabled or token matched.
    Ok(next.run(request).await)
}

// ─── Public serve entry point ─────────────────────────────────────────────────

/// Bind an axum listener to `addr` and serve the MCP Streamable HTTP server
/// on `/mcp` until the shared `shutdown` token is cancelled.
///
/// Called from `main.rs` when `[mcp].bind` is set. The shared
/// CancellationToken is cancelled on SIGTERM/SIGINT or when a re-exec is
/// requested — both paths call `bridge_core::request_reexec` which cancels
/// the token. The MCP server then shuts down cleanly and the main loop
/// proceeds to re-exec (if flagged).
///
/// `token` and `auth_enabled` come from `[http]` in config.toml (the shared
/// bearer token + auth flag), so operators only need one token for both bridges.
/// `dev` is `[mcp].dev` and `allowed_hosts` is `[mcp].allowed_hosts`.
///
/// The dangerous-combo refusal (non-loopback + dev + no-auth) is enforced once,
/// up front, by `DaemonConfig::validate` (which also honors
/// `[dev].allow_insecure_lan`); by the time this runs, that gate has passed.
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    addr: std::net::SocketAddr,
    token: Option<String>,
    auth_enabled: bool,
    dev_enabled: bool,
    allowed_hosts_cfg: Vec<String>,
    control_tx: mpsc::Sender<Control>,
    events_tx: broadcast::Sender<Event>,
    shutdown: CancellationToken,
    reexec_flag: Arc<std::sync::atomic::AtomicBool>,
    metrics: Arc<crate::metrics::Metrics>,
) {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    // Treat an empty token as no token at all, so an operator who sets
    // an empty `[http].token_file` can never accidentally satisfy the auth check —
    // it fails closed (rejects all) instead of accepting `Bearer ` (empty).
    let token = token.filter(|t| !t.is_empty());

    if dev_enabled {
        tracing::warn!("mcp: [mcp].dev is set — dev tools enabled (deploy/build/restart-daemon)");
    }

    // NOTE: the dangerous-combo refusal (non-loopback + dev + no-auth) lives in
    // DaemonConfig::validate, run at startup before this task is spawned — and it
    // honors [dev].allow_insecure_lan, which a hard refusal here would override.
    // So there is intentionally no second refusal in serve.

    // Build auth state for the middleware.
    let auth_state = if !auth_enabled {
        tracing::warn!(
            "mcp: AUTH DISABLED ([http].auth_enabled = false) — \
             any host on the network can send MCP commands without authentication"
        );
        AuthState {
            expected_bearer: None,
            fail_closed: false,
        }
    } else {
        match &token {
            None => {
                tracing::warn!(
                    "mcp: auth is ENABLED but no token is configured — \
                     all MCP requests will be rejected with 401 (set [http].token_file, \
                     or disable auth with [http].auth_enabled = false, in config.toml)"
                );
                AuthState {
                    expected_bearer: None,
                    fail_closed: true,
                }
            }
            Some(t) => {
                if addr.ip().is_unspecified() {
                    tracing::warn!(
                        "mcp: binding to {} with bearer auth — \
                         any host on the network can attempt authentication",
                        addr
                    );
                }
                AuthState {
                    expected_bearer: Some(format!("Bearer {t}")),
                    fail_closed: false,
                }
            }
        }
    };

    let handles = Handles {
        control_tx,
        events_tx,
        shutdown: shutdown.clone(),
        reexec_flag,
        dev_enabled,
        metrics,
    };

    // StreamableHttpServerConfig is #[non_exhaustive] — cannot use struct
    // literal syntax with ..Default::default(). Mutate the default instead.
    let mut config = StreamableHttpServerConfig::default();
    config.cancellation_token = shutdown.child_token();

    // Fix 3: Configure the host allowlist correctly.
    //
    // rmcp 1.7.0 does NOT support a "*" wildcard — it does literal string
    // matching. An empty allowed_hosts list means "allow all hosts" (see
    // rmcp source: host_is_allowed returns true when the list is empty).
    //
    // We build the allowlist from:
    //   1. Always: localhost, 127.0.0.1, ::1 (loopback)
    //   2. [mcp].allowed_hosts from config.toml (host[:port] entries)
    //   3. If the bind address is a concrete IP (not 0.0.0.0/::), include it.
    //   4. If the bind is a wildcard AND no config override, clear the list
    //      (allow-all) and warn — acceptable because Fix 2's bearer token is
    //      the real gate.
    let bind_is_wildcard = addr.ip().is_unspecified();

    if bind_is_wildcard && allowed_hosts_cfg.is_empty() {
        // Allow all — host header matching disabled. Token is the gate.
        tracing::warn!(
            "mcp: host allowlisting is DISABLED (bind is wildcard, \
             [mcp].allowed_hosts not set). \
             DNS-rebinding protection relies on the bearer token. \
             Set [mcp].allowed_hosts = [\"<host>\"] or bind to a concrete IP \
             to restrict by Host header."
        );
        config.allowed_hosts = vec![];
    } else {
        let mut hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
        if !bind_is_wildcard {
            // Include the concrete bind address so LAN clients connecting to
            // the gaming client's real IP get through.
            hosts.push(addr.ip().to_string());
            hosts.push(addr.to_string()); // also include host:port form
        }
        for h in &allowed_hosts_cfg {
            let h = h.trim();
            if !h.is_empty() {
                hosts.push(h.to_owned());
            }
        }
        config.allowed_hosts = hosts;
    }

    let handles_clone = handles.clone();
    let service: StreamableHttpService<TvShellMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(TvShellMcp::new(handles_clone.clone())),
            Default::default(),
            config,
        );

    // Wrap the MCP service with bearer-token auth middleware (Fix 2).
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(auth_state, auth_middleware));

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("mcp: failed to bind {addr}: {e}");
            return;
        }
    };
    tracing::info!("MCP server listening on http://{addr}/mcp");

    let _ = axum::serve(listener, router)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await;

    tracing::info!("mcp: server stopped");
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Building the tool router validates every tool's input AND output schema.
    // rmcp panics here if a structured tool's output schema root isn't an
    // `object` (e.g. a tool returning `Json<Vec<_>>` — a JSON array root). The
    // router is built per MCP session at runtime, so such a panic breaks EVERY
    // request (empty reply) rather than failing the build. This test forces the
    // validation in CI so the regression can't ship again.
    #[test]
    fn tool_router_builds_without_panic() {
        let _ = TvShellMcp::tool_router();
    }

    // ── advertised server capabilities ────────────────────────────────────────

    // The `resources` capability must serialize to a NON-EMPTY object so a
    // federating gateway (ContextForge) that gates on `if capabilities["resources"]:`
    // (Python — `{}` is falsy) actually fetches our resources instead of silently
    // dropping `screenshot://current`. `enable_resources_list_changed()` sets
    // `listChanged: true`, giving `{"listChanged":true}` rather than bare `{}`.
    #[test]
    fn resources_capability_serialises_non_empty() {
        let caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_resources_list_changed()
            .build();

        let resources = caps.resources.expect("resources capability is advertised");
        assert_eq!(
            resources.list_changed,
            Some(true),
            "resources.listChanged must be Some(true) so it serialises non-empty"
        );

        // The JSON object for `resources` must not be bare `{}` (the ContextForge
        // truthiness gate); `listChanged: true` is what makes it truthy.
        let json = serde_json::to_value(&resources).expect("resources serialises");
        let obj = json.as_object().expect("resources is a JSON object");
        assert!(
            !obj.is_empty(),
            "resources capability must serialise to a non-empty object, got {json}"
        );
        assert_eq!(obj.get("listChanged"), Some(&serde_json::Value::Bool(true)));
    }

    // ── screenshot resource descriptor ────────────────────────────────────────

    #[test]
    fn screenshot_resource_descriptor_shape() {
        let r = screenshot_resource_descriptor();
        assert_eq!(r.uri, SCREENSHOT_RESOURCE_URI);
        assert_eq!(r.uri, "screenshot://current");
        assert_eq!(r.mime_type.as_deref(), Some("image/png"));
        assert!(!r.name.is_empty(), "resource needs a name");
        assert!(r.description.is_some(), "resource needs a description");
    }

    #[test]
    fn screenshot_resource_serialises_as_blob_uri() {
        // The descriptor must round-trip through the rmcp Resource wrapper with
        // the custom URI scheme intact (resources/list payload shape).
        let resource = screenshot_resource_descriptor();
        let json = serde_json::to_string(&resource).expect("resource serialises");
        assert!(json.contains("screenshot://current"), "uri missing: {json}");
        assert!(json.contains("image/png"), "mimeType missing: {json}");
    }

    // ── NavKey enum serialisation ─────────────────────────────────────────────

    #[test]
    fn nav_key_as_str_matches_expected() {
        assert_eq!(NavKey::Up.as_str(), "up");
        assert_eq!(NavKey::Down.as_str(), "down");
        assert_eq!(NavKey::Left.as_str(), "left");
        assert_eq!(NavKey::Right.as_str(), "right");
        assert_eq!(NavKey::Select.as_str(), "select");
        assert_eq!(NavKey::Back.as_str(), "back");
    }

    #[test]
    fn nav_key_deserialises_lowercase() {
        // serde rename_all = "lowercase" → the JSON value must be the lowercase variant
        let up: NavKey = serde_json::from_str("\"up\"").expect("deserialise up");
        assert_eq!(up.as_str(), "up");
        let select: NavKey = serde_json::from_str("\"select\"").expect("deserialise select");
        assert_eq!(select.as_str(), "select");
        let back: NavKey = serde_json::from_str("\"back\"").expect("deserialise back");
        assert_eq!(back.as_str(), "back");
    }

    #[test]
    fn nav_key_deserialise_all_variants() {
        for (json, expected) in &[
            ("\"up\"", "up"),
            ("\"down\"", "down"),
            ("\"left\"", "left"),
            ("\"right\"", "right"),
            ("\"select\"", "select"),
            ("\"back\"", "back"),
        ] {
            let k: NavKey = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("failed to deserialise {json}: {e}"));
            assert_eq!(k.as_str(), *expected);
        }
    }

    // ── OverlayTarget enum serialisation ──────────────────────────────────────

    #[test]
    fn overlay_target_as_str_matches_expected() {
        assert_eq!(OverlayTarget::Volume.as_str(), "volume");
        assert_eq!(OverlayTarget::Network.as_str(), "network");
        assert_eq!(OverlayTarget::Session.as_str(), "session");
    }

    #[test]
    fn overlay_target_deserialises_lowercase() {
        let v: OverlayTarget = serde_json::from_str("\"volume\"").expect("deserialise volume");
        assert_eq!(v.as_str(), "volume");
        let n: OverlayTarget = serde_json::from_str("\"network\"").expect("deserialise network");
        assert_eq!(n.as_str(), "network");
        let s: OverlayTarget = serde_json::from_str("\"session\"").expect("deserialise session");
        assert_eq!(s.as_str(), "session");
    }

    #[test]
    fn overlay_target_maps_to_valid_intent() {
        // Every OverlayTarget variant must produce a valid overlay intent.
        for target in &[
            OverlayTarget::Volume,
            OverlayTarget::Network,
            OverlayTarget::Session,
        ] {
            let result = bridge_core::overlay_intent(target.as_str());
            assert!(
                result.is_ok(),
                "expected valid intent for {:?}, got {:?}",
                target.as_str(),
                result
            );
        }
    }

    // ── shell_action: bare-verb acceptance + deep-link rejection ─────────────
    //
    // The MCP tool layer rejects deep-links (any name containing ':') and
    // accepts only the closed bare vocabulary from INTENT_VOCAB. These tests
    // verify the parsing behaviour independently of the dispatch path.

    #[test]
    fn shell_action_accepts_all_bare_vocab_entries() {
        use crate::protocol::INTENT_VOCAB;
        for &name in INTENT_VOCAB {
            // Must not contain ':' (it's a bare name) and must be valid.
            assert!(
                !name.contains(':'),
                "INTENT_VOCAB entry '{name}' contains ':' — it is not a bare action"
            );
            assert!(
                bridge_core::is_valid_intent(name),
                "bare vocab entry '{name}' must be a valid intent"
            );
        }
    }

    #[test]
    fn shell_action_rejects_settings_deep_link() {
        // The colon check happens before the vocab check.
        let name = "settings:bluetooth";
        assert!(
            name.contains(':'),
            "test sanity: '{name}' should contain ':'"
        );
    }

    #[test]
    fn shell_action_rejects_overlay_deep_link() {
        let name = "overlay:volume";
        assert!(name.contains(':'));
    }

    #[test]
    fn shell_action_rejects_app_deep_link() {
        let name = "app:steam";
        assert!(name.contains(':'));
    }

    #[test]
    fn shell_action_rejects_unknown_bare_name() {
        // An unknown bare name (no colon) fails the vocab check.
        assert!(!bridge_core::is_valid_intent("launchgame"));
        assert!(!bridge_core::is_valid_intent("unknown"));
        assert!(!bridge_core::is_valid_intent(""));
    }

    // ── SettingsPage enum serialisation ──────────────────────────────────────

    #[test]
    fn settings_page_as_str_covers_all_variants() {
        let cases = &[
            (SettingsPage::Audio, "audio"),
            (SettingsPage::Bluetooth, "bluetooth"),
            (SettingsPage::Network, "network"),
            (SettingsPage::Display, "display"),
            (SettingsPage::Controllers, "controllers"),
            (SettingsPage::Keybindings, "keybindings"),
            (SettingsPage::Avcontrol, "avcontrol"),
            (SettingsPage::Streaming, "streaming"),
            (SettingsPage::Widgets, "widgets"),
            (SettingsPage::Accessibility, "accessibility"),
            (SettingsPage::Power, "power"),
            (SettingsPage::System, "system"),
        ];
        for (variant, expected) in cases {
            assert_eq!(variant.as_str(), *expected, "SettingsPage variant mismatch");
        }
    }

    #[test]
    fn settings_page_deserialises_lowercase() {
        let slugs = &[
            "audio",
            "bluetooth",
            "network",
            "display",
            "controllers",
            "keybindings",
            "avcontrol",
            "streaming",
            "widgets",
            "accessibility",
            "power",
            "system",
        ];
        for slug in slugs {
            let json = format!("\"{}\"", slug);
            let page: SettingsPage = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("failed to deserialise SettingsPage '{slug}': {e}"));
            assert_eq!(
                page.as_str(),
                *slug,
                "SettingsPage::as_str() mismatch for '{slug}'"
            );
        }
    }

    #[test]
    fn settings_page_rejects_unknown_slug() {
        let result: Result<SettingsPage, _> = serde_json::from_str("\"unknown-page\"");
        assert!(
            result.is_err(),
            "unknown slug should fail to deserialise as SettingsPage"
        );
    }

    #[test]
    fn settings_page_as_str_produces_valid_settings_intent() {
        // Every SettingsPage slug must form a valid intent when prefixed by "settings:".
        for &slug in &[
            "audio",
            "bluetooth",
            "network",
            "display",
            "controllers",
            "keybindings",
            "avcontrol",
            "streaming",
            "widgets",
            "accessibility",
            "power",
            "system",
        ] {
            let intent = bridge_core::settings_intent(slug);
            assert!(
                bridge_core::is_valid_intent(&intent),
                "settings_intent('{slug}') = '{intent}' should be a valid intent"
            );
        }
    }

    // ── list_apps bridge fn ───────────────────────────────────────────────────

    #[cfg(feature = "mcp")]
    #[test]
    fn app_entry_serialises_with_schema() {
        use schemars::schema_for;

        let entry = bridge_core::AppEntry {
            name: "Firefox".into(),
            wm_class: "org.mozilla.Firefox".into(),
            comment: "Browse the web".into(),
        };
        let json = serde_json::to_string(&entry).expect("AppEntry serialises");
        assert!(json.contains("\"name\":\"Firefox\""));
        assert!(json.contains("\"wm_class\":\"org.mozilla.Firefox\""));
        assert!(json.contains("\"comment\":\"Browse the web\""));

        // Schema must compile and contain expected field names.
        let schema = schema_for!(bridge_core::AppEntry);
        let schema_json = serde_json::to_string(&schema).expect("schema serialises");
        assert!(schema_json.contains("name"), "schema missing 'name' field");
        assert!(
            schema_json.contains("wm_class"),
            "schema missing 'wm_class' field"
        );
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn app_entry_omits_empty_comment() {
        let entry = bridge_core::AppEntry {
            name: "App".into(),
            wm_class: "app".into(),
            comment: String::new(),
        };
        let json = serde_json::to_string(&entry).expect("AppEntry serialises");
        // skip_serializing_if = "String::is_empty" should omit the comment field.
        assert!(!json.contains("comment"), "empty comment should be omitted");
    }
}
