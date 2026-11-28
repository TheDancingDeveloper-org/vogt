//! A long-lived Claude Code login per account (WI-4832).
//!
//! Claude OAuth refresh tokens are single-use. Every pod used to hold its own
//! copy of an account's `.credentials.json`, so the first session to refresh
//! invalidated the copy in every other pod and each showed "Login expired".
//! `claude setup-token` mints a token (about a year) that Claude Code uses
//! directly, with no refresh, from `CLAUDE_CODE_OAUTH_TOKEN`.
//!
//! The engine reads one variable per account,
//! `ENGINE_AGENT_CLAUDE_OAUTH_TOKEN_<ACCOUNT>` (the account name upper-cased,
//! `-` as `_`; `acct1` is `..._ACCT1`) from the engine's own process
//! environment (on vogt-dev, an ExternalSecret through an optional
//! `secretKeyRef`), not from the agent-auth manifest, which only brokers secrets
//! into session children. It gives the value as `CLAUDE_CODE_OAUTH_TOKEN` only to a Claude Code
//! session running under that account. The value is never logged and never
//! travels in argv or a literal pod `EnvVar`.

/// The variable Claude Code reads the token from.
pub const CLAUDE_CODE_OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// The prefix of the engine's per-account variable.
const ENGINE_TOKEN_ENV_PREFIX: &str = "ENGINE_AGENT_CLAUDE_OAUTH_TOKEN_";

/// The engine variable that holds `account`'s token.
pub fn engine_env_name(account: &str) -> String {
    let suffix: String = account
        .chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    format!("{ENGINE_TOKEN_ENV_PREFIX}{suffix}")
}

/// [`token_for`] over an explicit environment lookup.
pub fn token_from(account: &str, get: impl Fn(&str) -> Option<String>) -> Option<String> {
    get(&engine_env_name(account))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The account's token from the engine's own environment, if one is set and
/// not blank. The value is the caller's to keep out of logs.
pub fn token_for(account: Option<&str>) -> Option<String> {
    token_from(account?, |name| std::env::var(name).ok())
}

/// The token a session gets, or `None`: only a Claude Code command that runs
/// under a named account is given that account's token. A shell, Codex,
/// OpenCode, a session with no account, and every other account get nothing.
pub fn token_for_session(
    account: Option<&str>,
    command: Option<&[String]>,
    get: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let command = command?;
    (crate::agent_cli::agent_name(command).as_deref() == Some("claude"))
        .then(|| token_from(account?, get))
        .flatten()
}

/// Whether the engine holds a token for `account`. Never reads the value out.
pub fn is_configured(account: Option<&str>) -> bool {
    token_for(account).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_name_is_the_upper_cased_account() {
        assert_eq!(
            engine_env_name("acct1"),
            "ENGINE_AGENT_CLAUDE_OAUTH_TOKEN_ACCT1"
        );
        assert_eq!(
            engine_env_name("team-b_2"),
            "ENGINE_AGENT_CLAUDE_OAUTH_TOKEN_TEAM_B_2"
        );
    }

    #[test]
    fn only_the_named_accounts_variable_is_read_and_blank_is_unset() {
        let env = |name: &str| match name {
            "ENGINE_AGENT_CLAUDE_OAUTH_TOKEN_ACCT1" => Some(" tok \n".to_string()),
            "ENGINE_AGENT_CLAUDE_OAUTH_TOKEN_BLANK" => Some("  ".to_string()),
            _ => None,
        };
        assert_eq!(token_from("acct1", env).as_deref(), Some("tok"));
        assert_eq!(token_from("acct2", env), None);
        assert_eq!(token_from("blank", env), None);
    }

    fn claude() -> Vec<String> {
        vec!["claude".into()]
    }

    fn env(name: &str) -> Option<String> {
        (name == "ENGINE_AGENT_CLAUDE_OAUTH_TOKEN_ACCT1").then(|| "tok".to_string())
    }

    #[test]
    fn a_claude_session_under_the_account_gets_its_token() {
        assert_eq!(
            token_for_session(Some("acct1"), Some(&claude()), env).as_deref(),
            Some("tok")
        );
        let wrapped: Vec<String> = ["vogt-agent-auth", "run", "--", "claude"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            token_for_session(Some("acct1"), Some(&wrapped), env).as_deref(),
            Some("tok")
        );
    }

    #[test]
    fn other_sessions_get_no_token() {
        // Another account, no account, a non-Claude command, no command.
        assert_eq!(token_for_session(Some("acct2"), Some(&claude()), env), None);
        assert_eq!(token_for_session(None, Some(&claude()), env), None);
        for other in [["codex"], ["bash"], ["opencode"]] {
            let argv: Vec<String> = other.map(String::from).to_vec();
            assert_eq!(token_for_session(Some("acct1"), Some(&argv), env), None);
        }
        assert_eq!(token_for_session(Some("acct1"), None, env), None);
    }

    #[test]
    fn no_account_has_no_token() {
        assert_eq!(token_for(None), None);
    }
}
