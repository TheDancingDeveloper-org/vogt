//! When an oversight session may answer a worker's permission prompt
//! (WI-1831, widened by WI-2028).
//!
//! `person_gate` keeps every permission prompt a person's to answer. That
//! sent every prompt a driven worker hit — Codex's "Would you like to run the
//! following command?" above all — to the operator's Inbox. The operator's
//! decision (2026-10-20) is that oversight answers them: a session the engine
//! has marked oversight may choose any option on a worker's **permission**
//! prompt. A read outside the working directories and every other caller
//! still need a person.
//!
//! The decision is made in the engine from facts it reads itself: the dialog
//! on screen and the overseer's role (`api::oversight_may_answer`). The core
//! only says which session it is relaying (`overseer` on the answer and input
//! requests), honoured only from the stack secret, like `person`. Every answer
//! is audited as the oversight session, with the command excerpt.

use crate::approval::Detected;

/// Why an oversight answer was refused, for the refusal text and the audit.
pub type Refusal = String;

/// The longest command excerpt an audit line carries.
const EXCERPT_MAX: usize = 300;

/// Decide whether an oversight session may answer `dialog`. `Ok` carries the
/// command excerpt, cut for the audit line.
pub fn may_answer(dialog: &Detected) -> Result<String, Refusal> {
    if dialog.kind != "permission" {
        return Err(format!("a {} prompt is a person's to answer", dialog.kind));
    }
    let excerpt = dialog.command_excerpt.trim();
    Ok(if excerpt.chars().count() > EXCERPT_MAX {
        let mut cut: String = excerpt.chars().take(EXCERPT_MAX).collect();
        cut.push('…');
        cut
    } else {
        excerpt.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(kind: &'static str, excerpt: &str) -> Detected {
        Detected {
            question: "Would you like to run the following command?".into(),
            command_excerpt: excerpt.into(),
            deadline_seconds: None,
            kind,
            options: Vec::new(),
            horizontal: false,
        }
    }

    #[test]
    fn oversight_answers_any_permission_prompt_and_nothing_else() {
        let codex = dialog("permission", "  $ cargo test -p vogt-engine  ");
        assert_eq!(
            may_answer(&codex).as_deref(),
            Ok("$ cargo test -p vogt-engine")
        );
        // Not only the old allow-list: a merge is oversight's call now.
        let merge = dialog("permission", "Bash command\n  gh pr merge 3\n  Merge");
        assert!(may_answer(&merge).is_ok());
        assert!(may_answer(&dialog("read-outside-cwd", "/etc/passwd")).is_err());
        assert!(may_answer(&dialog("folder-trust", "")).is_err());
    }

    #[test]
    fn a_long_excerpt_is_cut_for_the_audit() {
        let long = "x".repeat(1000);
        let cut = may_answer(&dialog("permission", &long)).unwrap();
        assert_eq!(cut.chars().count(), EXCERPT_MAX + 1);
        assert!(cut.ends_with('…'));
    }
}
