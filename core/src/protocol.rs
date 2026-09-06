//! Wire protocol — command parsing and reply builders.
//!
//! V2_DESIGN §4 lists "the Unix-socket IPC framing and reply grammar" among the
//! things **carried over unchanged in contract** from v1. So this module mirrors
//! `daemon/src/protocol.rs` exactly rather than inventing anything:
//!
//! * **Framing**: newline-delimited text, one command per line, one reply line
//!   per command, 4096-byte maximum line.
//! * **Tokenization**: whitespace-split, no quoting. A verb with a body must be
//!   followed by whitespace, so `screen-stateX` is not `screen-state`.
//! * **Replies**: `ok` · `unknown` · `error:<msg>` · a bare compact JSON document
//!   as the whole line for a payload. There is no envelope and no `ok ` prefix
//!   on a JSON reply.
//! * Anything interpolated into an error is run through [`sanitize_ipc`], because
//!   an embedded newline would split one reply into two lines and desync a
//!   line-reading client.

use std::fmt;

/// Maximum accepted line length, matching v1's `LinesCodec::new_with_max_length`.
pub const MAX_LINE: usize = 4096;

/// One parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Liveness. Replies `ok`.
    Ping,
    /// One `ScreenState` snapshot as compact JSON.
    ScreenState,
    /// Put an app on screen.
    Show(String),
    /// `show` with no argument.
    ShowUsage,
    /// Return to the shell.
    Home,
    /// Launch a command in a scope for an app id.
    Launch {
        app_id: String,
        command: Vec<String>,
    },
    /// `launch` with too few arguments.
    LaunchUsage,
    /// Not a verb this core knows.
    Unknown,
}

impl Command {
    /// Parse one line. The trailing newline is already stripped by the codec;
    /// surrounding whitespace is trimmed, mirroring v1.
    pub fn parse(line: &str) -> Command {
        let cmd = line.trim();
        match cmd {
            "ping" => return Command::Ping,
            "screen-state" => return Command::ScreenState,
            "home" => return Command::Home,
            // A bare verb that requires a body is a usage error, not `unknown`:
            // the client asked for something real and got the arity wrong.
            "show" => return Command::ShowUsage,
            "launch" => return Command::LaunchUsage,
            _ => {}
        }
        if let Some(body) = command_body(cmd, "show") {
            return if body.is_empty() {
                Command::ShowUsage
            } else {
                Command::Show(body.to_string())
            };
        }
        if let Some(body) = command_body(cmd, "launch") {
            let mut parts = body.split_whitespace();
            let Some(app_id) = parts.next() else {
                return Command::LaunchUsage;
            };
            let command: Vec<String> = parts.map(str::to_string).collect();
            if command.is_empty() {
                return Command::LaunchUsage;
            }
            return Command::Launch {
                app_id: app_id.to_string(),
                command,
            };
        }
        Command::Unknown
    }
}

/// If `cmd` is `verb` followed by whitespace, return the trimmed remainder.
///
/// The word-boundary check is what keeps `show-something` from being parsed as
/// `show` with the body `-something`.
fn command_body<'a>(cmd: &'a str, verb: &str) -> Option<&'a str> {
    let rest = cmd.strip_prefix(verb)?;
    match rest.chars().next() {
        None => Some(""),
        Some(c) if c.is_whitespace() => Some(rest.trim()),
        Some(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Response builders (the exact reply strings, sans trailing newline).
// ---------------------------------------------------------------------------

/// Success with no payload.
pub fn resp_ok() -> String {
    "ok".to_string()
}

/// The client sent a verb this core does not have.
pub fn resp_unknown() -> String {
    "unknown".to_string()
}

/// Every error reply. `msg` is free text, sanitized to one line.
pub fn resp_error(msg: &str) -> String {
    format!("error:{}", sanitize_ipc(msg))
}

/// Wrong arity or a missing body.
pub fn resp_usage(usage: &str) -> String {
    resp_error(&format!("usage: {usage}"))
}

/// Serialize a payload as the whole reply line, degrading to an error reply.
pub fn resp_json<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_string(value) {
        Ok(json) => sanitize_ipc(&json),
        Err(e) => resp_error(&format!("serialize failed: {e}")),
    }
}

/// Strip control characters from anything destined for a reply line.
///
/// The wire protocol is newline-delimited and clients read it line-by-line, so a
/// `\n` or `\r` embedded in an error body (an error `Display` string, a file
/// value echoed back) would split one reply into several and desync the client.
/// Keeping every reply on one line preserves framing. Pure — unit-tested.
pub fn sanitize_ipc(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// An event line. Kept as an enum whose `Display` **is** the wire format, as in
/// v1, so the format lives in exactly one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The base layer changed to this app id.
    BaseLayer(String),
    /// A base-layer switch was asked for and the compositor did not publish it.
    /// An event, not just an error reply, because §10 requires the failure to be
    /// observable by something other than the client that provoked it.
    SwitchFailed(String),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::BaseLayer(id) => write!(f, "baselayer:{id}"),
            Event::SwitchFailed(msg) => write!(f, "baselayer:failed:{}", sanitize_ipc(msg)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_verbs_parse() {
        assert_eq!(Command::parse("ping"), Command::Ping);
        assert_eq!(Command::parse("screen-state"), Command::ScreenState);
        assert_eq!(Command::parse("home"), Command::Home);
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(Command::parse("  ping  "), Command::Ping);
        assert_eq!(Command::parse("\thome"), Command::Home);
    }

    #[test]
    fn show_takes_one_argument() {
        assert_eq!(Command::parse("show 9003"), Command::Show("9003".into()));
        assert_eq!(
            Command::parse("show   9003  "),
            Command::Show("9003".into())
        );
    }

    #[test]
    fn a_bare_body_verb_is_a_usage_error_not_unknown() {
        assert_eq!(Command::parse("show"), Command::ShowUsage);
        assert_eq!(Command::parse("show "), Command::ShowUsage);
        assert_eq!(Command::parse("launch"), Command::LaunchUsage);
        assert_eq!(Command::parse("launch 9003"), Command::LaunchUsage);
    }

    #[test]
    fn word_boundaries_are_enforced() {
        // `showX` must not be `show` with body `X`.
        assert_eq!(Command::parse("showX"), Command::Unknown);
        assert_eq!(Command::parse("show-me 1"), Command::Unknown);
        assert_eq!(Command::parse("launchpad 1 x"), Command::Unknown);
        assert_eq!(Command::parse("pingpong"), Command::Unknown);
    }

    #[test]
    fn launch_splits_the_command_on_whitespace() {
        assert_eq!(
            Command::parse("launch 9003 moonlight stream host app"),
            Command::Launch {
                app_id: "9003".into(),
                command: vec![
                    "moonlight".into(),
                    "stream".into(),
                    "host".into(),
                    "app".into()
                ],
            }
        );
    }

    #[test]
    fn unknown_verbs_are_unknown() {
        assert_eq!(Command::parse(""), Command::Unknown);
        assert_eq!(Command::parse("frobnicate"), Command::Unknown);
        // v1 verbs the core deliberately does not answer.
        assert_eq!(Command::parse("hypr-active"), Command::Unknown);
        assert_eq!(Command::parse("grab"), Command::Unknown);
    }

    #[test]
    fn reply_grammar_matches_v1() {
        assert_eq!(resp_ok(), "ok");
        assert_eq!(resp_unknown(), "unknown");
        assert_eq!(resp_error("boom"), "error:boom");
        assert_eq!(resp_usage("show <appid>"), "error:usage: show <appid>");
    }

    #[test]
    fn json_replies_are_the_whole_line_with_no_envelope() {
        let reply = resp_json(&serde_json::json!({"a": 1}));
        assert_eq!(reply, r#"{"a":1}"#);
        assert!(!reply.starts_with("ok"));
    }

    #[test]
    fn control_characters_never_reach_the_wire() {
        assert_eq!(sanitize_ipc("a\nb"), "a b");
        assert_eq!(sanitize_ipc("a\r\nb\tc"), "a  b c");
        assert_eq!(sanitize_ipc("plain"), "plain");
        // The framing invariant: no reply may contain a newline.
        for s in ["a\nb", "\r", "x\u{0}y"] {
            assert!(!resp_error(s).contains('\n'));
            assert!(!resp_error(s).contains('\r'));
        }
    }

    #[test]
    fn every_error_reply_is_prefixed_and_single_line() {
        let replies = [
            resp_error("x"),
            resp_usage("y"),
            // A map keyed by a tuple is a serde_json hard error ("key must be a
            // string"), so this exercises the degrade-to-error path in resp_json
            // rather than a happy value.
            resp_json(&std::collections::BTreeMap::from([((1u8, 2u8), 3u8)])),
        ];
        for r in replies {
            assert!(r.starts_with("error:"), "{r}");
            assert_eq!(r.lines().count(), 1, "{r}");
        }
    }

    #[test]
    fn event_display_is_the_wire_format() {
        assert_eq!(
            Event::BaseLayer("9003".into()).to_string(),
            "baselayer:9003"
        );
        assert_eq!(
            Event::SwitchFailed("did not\ntake".into()).to_string(),
            "baselayer:failed:did not take"
        );
    }

    #[test]
    fn max_line_matches_v1() {
        assert_eq!(MAX_LINE, 4096);
    }
}
