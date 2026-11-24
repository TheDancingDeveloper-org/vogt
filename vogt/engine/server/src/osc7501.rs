//! OSC 7501: the status a program reports about itself (WI-4321).
//!
//! `ESC ] 7501 ; key=value:key=value ST` (ST is `ESC \` or BEL). Design:
//! `docs/design/osc7501-session-status.md`. Anything running in the PTY can
//! emit this, so every field is validated, size-limited and treated as
//! untrusted text; a sequence with any invalid field is ignored whole. Message
//! text is never logged.

use std::borrow::Cow;

use base64::Engine as _;
use vogt_engine_contract::ReportedState;

/// Longest decoded `msg`, in bytes.
pub const MAX_MSG: usize = 2048;
/// Longest decoded `title`, in bytes.
pub const MAX_TITLE: usize = 192;
/// Longest `app`, in characters.
pub const MAX_APP: usize = 32;
/// Most `/`-separated parts in an `id`.
pub const MAX_ID_PARTS: usize = 8;
/// Longest whole payload accepted (base64 of `MAX_MSG` plus the other keys).
const MAX_PAYLOAD: usize = 4096;

/// What the engine says back to a capability query.
pub const QUERY_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// One parsed OSC 7501 sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc7501 {
    /// `ESC ] 7501 ; ? ST`: what does the terminal support?
    Query,
    /// `state=clear`: remove the reported status.
    Clear,
    Report(Report),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub state: ReportedState,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub msg: Option<String>,
    pub title: Option<String>,
    pub app: Option<String>,
    pub id: Option<String>,
}

/// Parse the payload after `7501;`. None for anything malformed or oversized.
pub fn parse(payload: &[u8]) -> Option<Osc7501> {
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    if payload == b"?" {
        return Some(Osc7501::Query);
    }
    let text = std::str::from_utf8(payload).ok()?;
    let (mut state, mut kind, mut progress) = (None, None, None);
    let (mut msg, mut title, mut app, mut id) = (None, None, None, None);
    for field in text.split(':') {
        // Split on the first `=` only: base64 padding is `=`.
        let (key, value) = field.split_once('=')?;
        match key {
            "state" => {
                if value == "clear" {
                    state = Some(None);
                } else {
                    state = Some(Some(match value {
                        "idle" => ReportedState::Idle,
                        "working" => ReportedState::Working,
                        "done" => ReportedState::Done,
                        "blocked" => ReportedState::Blocked,
                        "error" => ReportedState::Error,
                        _ => return None,
                    }));
                }
            }
            "kind" => {
                kind = Some(match value {
                    "permission" | "question" | "auth" => value.to_string(),
                    _ => return None,
                })
            }
            "progress" => {
                let n: u8 = value.parse().ok().filter(|n| *n <= 100)?;
                progress = Some(n);
            }
            "msg" => msg = Some(decode_text(value, MAX_MSG)?),
            "title" => title = Some(decode_text(value, MAX_TITLE)?),
            "app" => {
                if value.is_empty()
                    || value.chars().count() > MAX_APP
                    || !value.chars().all(name_char)
                {
                    return None;
                }
                app = Some(value.to_string());
            }
            "id" => {
                let parts: Vec<&str> = value.split('/').collect();
                if parts.len() > MAX_ID_PARTS
                    || parts
                        .iter()
                        .any(|p| p.is_empty() || p.len() > MAX_APP || !p.chars().all(name_char))
                {
                    return None;
                }
                id = Some(value.to_string());
            }
            // A key from a later revision of the protocol.
            _ => {}
        }
    }
    match state? {
        None => Some(Osc7501::Clear),
        Some(state) => Some(Osc7501::Report(Report {
            state,
            // `kind` and `progress` mean something only with some states.
            kind: kind.filter(|_| state == ReportedState::Blocked),
            progress: progress
                .filter(|_| matches!(state, ReportedState::Working | ReportedState::Blocked)),
            msg,
            title,
            app,
            id,
        })),
    }
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// Base64 to text, at most `max` decoded bytes, control characters dropped
/// (newline and tab kept) so the text cannot steer a terminal that shows it.
fn decode_text(value: &str, max: usize) -> Option<String> {
    // Reject by encoded length first so an oversized value is never decoded.
    if value.len() > max.div_ceil(3) * 4 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()?;
    if bytes.len() > max {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    Some(
        text.chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect(),
    )
}

/// Remove complete capability queries (`ESC ] 7501 ; ? BEL` or `... ESC \`)
/// from output headed for the replay ring and clients, so no client answers
/// the agent's question itself (WI-991). Best effort: a query split across two
/// reads passes through (the grid still sees it and the engine still replies;
/// a client ignores an OSC it does not know).
pub fn strip_queries(data: &[u8]) -> Cow<'_, [u8]> {
    const BEL: &[u8] = b"\x1b]7501;?\x07";
    const ST: &[u8] = b"\x1b]7501;?\x1b\\";
    if !data
        .windows(BEL.len() - 1)
        .any(|w| w == &BEL[..BEL.len() - 1])
    {
        return Cow::Borrowed(data);
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(BEL) {
            i += BEL.len();
        } else if data[i..].starts_with(ST) {
            i += ST.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Terminal;
    use base64::engine::general_purpose::STANDARD as B64;

    fn b64(s: &str) -> String {
        B64.encode(s)
    }

    #[test]
    fn parses_every_key() {
        let p = format!(
            "state=blocked:kind=permission:progress=40:msg={}:title={}:app=claude-code:id=a/b",
            b64("approve Write"),
            b64("T")
        );
        let Some(Osc7501::Report(r)) = parse(p.as_bytes()) else {
            panic!("not a report");
        };
        assert_eq!(r.state, ReportedState::Blocked);
        assert_eq!(r.kind.as_deref(), Some("permission"));
        assert_eq!(r.progress, Some(40));
        assert_eq!(r.msg.as_deref(), Some("approve Write"));
        assert_eq!(r.title.as_deref(), Some("T"));
        assert_eq!(r.app.as_deref(), Some("claude-code"));
        assert_eq!(r.id.as_deref(), Some("a/b"));
    }

    #[test]
    fn query_and_clear() {
        assert_eq!(parse(b"?"), Some(Osc7501::Query));
        assert_eq!(parse(b"state=clear"), Some(Osc7501::Clear));
    }

    #[test]
    fn malformed_and_oversized_are_ignored() {
        let big = |n: usize| b64(&"x".repeat(n));
        for bad in [
            "".to_string(),
            "state=nope".into(),
            "app=x".into(),
            "state=idle:kind=bogus".into(),
            "state=working:progress=101".into(),
            "state=working:progress=-1".into(),
            "state=idle:msg=!!!notbase64".into(),
            format!("state=idle:msg={}", big(MAX_MSG + 1)),
            format!("state=idle:title={}", big(MAX_TITLE + 1)),
            format!("state=idle:app={}", "a".repeat(MAX_APP + 1)),
            "state=idle:app=bad name".into(),
            "state=idle:id=1/2/3/4/5/6/7/8/9".into(),
            "state=idle:id=a//b".into(),
            "state=idle:novalue".into(),
            format!("state=idle:msg={}", "A".repeat(MAX_PAYLOAD)),
        ] {
            assert_eq!(parse(bad.as_bytes()), None, "{bad:.60}");
        }
        assert_eq!(parse(&[0xff, 0xfe]), None);
        // The limit itself is accepted.
        let ok = format!("state=idle:msg={}", big(MAX_MSG));
        assert!(matches!(parse(ok.as_bytes()), Some(Osc7501::Report(_))));
    }

    #[test]
    fn msg_control_characters_are_dropped() {
        let p = format!("state=idle:msg={}", b64("a\x1b[31mb\nc"));
        let Some(Osc7501::Report(r)) = parse(p.as_bytes()) else {
            panic!()
        };
        assert_eq!(r.msg.as_deref(), Some("a[31mb\nc"));
    }

    #[test]
    fn unknown_keys_and_inapplicable_fields_are_dropped() {
        let Some(Osc7501::Report(r)) = parse(b"state=idle:future=1:kind=question:progress=5")
        else {
            panic!()
        };
        assert_eq!((r.kind, r.progress), (None, None));
    }

    #[test]
    fn strips_queries_only() {
        assert_eq!(
            &*strip_queries(b"a\x1b]7501;?\x07b\x1b]7501;?\x1b\\c"),
            b"abc"
        );
        let keep = b"x\x1b]7501;state=idle\x07y";
        assert!(matches!(strip_queries(keep), Cow::Borrowed(_)));
        assert_eq!(&*strip_queries(b"\x1b]7501;?\x07"), b"");
    }

    fn fixture(name: &str) -> Vec<String> {
        let path = format!(
            "{}/tests/fixtures/osc7501/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        v["sequences"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    }

    /// Replay a capture through the real terminal, returning the reported
    /// state after each sequence and whether a query was seen.
    fn replay(name: &str) -> (Vec<Option<ReportedState>>, bool) {
        let mut term = Terminal::new(24, 80);
        let (mut states, mut queried) = (Vec::new(), false);
        for seq in fixture(name) {
            term.process(seq.as_bytes());
            queried |= term.take_query();
            states.push(term.reported().map(|r| r.state));
        }
        (states, queried)
    }

    #[test]
    fn replays_the_fixtures() {
        use ReportedState::*;
        let (states, queried) = replay("claude-code-2.1.322-permission-answered.json");
        assert!(queried);
        assert_eq!(states.last().unwrap(), &Some(Idle));
        assert!(states.contains(&Some(Blocked)));
        let mut term = Terminal::new(24, 80);
        for seq in fixture("claude-code-2.1.322-permission-answered.json") {
            term.process(seq.as_bytes());
            if term.reported().is_some_and(|r| r.state == Blocked) {
                let r = term.reported().unwrap();
                assert_eq!(r.kind.as_deref(), Some("permission"));
                assert_eq!(r.app.as_deref(), Some("claude-code"));
                assert!(r.msg.unwrap().starts_with("approve Write"));
            }
        }
        let (states, _) = replay("claude-code-2.1.322-error-bad-model-answered.json");
        assert!(states.contains(&Some(Error)));
        let (states, _) = replay("claude-code-2.1.322-turn-answered.json");
        assert!(states.contains(&Some(Working)) && states.contains(&Some(Done)));
        // Unanswered captures and agents that never report leave no status.
        for name in [
            "claude-code-2.1.289-turn-unanswered.json",
            "codex-fork-0.162.0-turn-backend-error.json",
            "codex-upstream-0.164.0-startup.json",
        ] {
            let (states, _) = replay(name);
            assert!(states.iter().all(Option::is_none), "{name}");
        }
    }

    #[test]
    fn every_fixture_replays_without_panicking() {
        let dir = format!("{}/tests/fixtures/osc7501", env!("CARGO_MANIFEST_DIR"));
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if name.ends_with(".json") {
                replay(&name);
            }
        }
    }
}
