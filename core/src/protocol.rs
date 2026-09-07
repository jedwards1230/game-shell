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

/// Maximum accepted line length, matching v1's `LinesCodec::new_with_max_length`.
pub const MAX_LINE: usize = 4096;

/// One parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Liveness. Replies `ok`.
    Ping,
    /// One `ScreenState` snapshot as compact JSON.
    ScreenState,
    /// One `InputReport` snapshot as compact JSON: the pad fleet, the player
    /// slots, the presenter devices, and what discovery is refusing.
    InputState,
    /// Put an app on screen.
    Show(String),
    /// `show` with no argument.
    ShowUsage,
    /// Return to the shell.
    Home,
    /// Launch an app in a scope.
    ///
    /// `command` EMPTY means "use the `[[app]]` class for this id" — the default
    /// path, and the only one that cannot forget the class environment. A
    /// non-empty `command` is the ad-hoc form; it still picks up the class
    /// environment when the id names a class (see `compositor::resolve_launch`).
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
            "input-state" => return Command::InputState,
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
            // An EMPTY command is no longer a usage error: it is the class form,
            // `launch <appid>`, which the compositor resolves against `[[app]]`.
            // It used to be rejected here, which forced every caller — the boot
            // client included — to repeat the argv AND the environment, so a
            // second place could forget the `WAYLAND_DISPLAY` unset that decides
            // whether Moonlight maps a window at all.
            let command: Vec<String> = parts.map(str::to_string).collect();
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

// NOTE: there is deliberately no `Event` type here yet. An event line only means
// something once something BROADCASTS it, and this PR ships no event stream and
// no metrics surface (both are explicitly later work — see the crate docs). A
// type nothing constructs is dead code dressed as a contract, and the repo
// deletes dead code rather than parking it. The wire format is written down in
// §4/§10 and comes back with the transport that carries it.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_verbs_parse() {
        assert_eq!(Command::parse("ping"), Command::Ping);
        assert_eq!(Command::parse("screen-state"), Command::ScreenState);
        assert_eq!(Command::parse("input-state"), Command::InputState);
        assert_eq!(Command::parse("home"), Command::Home);
    }

    /// `input-state` is a bare read verb: it takes no body, so anything glued to
    /// it is a different word and must not be mistaken for it.
    #[test]
    fn input_state_takes_no_body() {
        assert_eq!(Command::parse("  input-state "), Command::InputState);
        assert_eq!(Command::parse("input-stateX"), Command::Unknown);
        assert_eq!(Command::parse("input-state 1"), Command::Unknown);
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
    }

    /// `launch <appid>` with no command is the CLASS form, not a usage error.
    ///
    /// It used to be `LaunchUsage`, which forced every caller to repeat the argv
    /// and — the part that actually broke on hardware — the class environment.
    /// The arity error is now only the truly ambiguous case: no app id at all.
    #[test]
    fn launch_with_only_an_app_id_is_the_class_form() {
        assert_eq!(
            Command::parse("launch 9003"),
            Command::Launch {
                app_id: "9003".to_string(),
                command: vec![],
            }
        );
        assert_eq!(
            Command::parse("launch  9003  "),
            Command::Launch {
                app_id: "9003".to_string(),
                command: vec![],
            }
        );
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
    fn max_line_matches_v1() {
        assert_eq!(MAX_LINE, 4096);
    }
}
