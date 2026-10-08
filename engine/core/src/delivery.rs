//! Did typed input land? Ports `src/vogt/core/delivery.py`.
//!
//! Judged from what the engine reported before the input and a few reads of
//! the screen after it. Evidence, not a promise.

#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Typed,
    Delivered,
    Queued,
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub activity: Option<String>,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub delivery: Delivery,
    pub evidence: String,
}

pub fn judge(submitted: bool, before: Option<&str>, after: &[Observation]) -> Verdict {
    if !submitted {
        return Verdict {
            delivery: Delivery::Typed,
            evidence: "no Enter was pressed: the text is in the input, not sent".to_string(),
        };
    }
    let hint = regex::Regex::new(r"(?i)queued messages?|message(?:s)? queued").expect("constant");
    if after
        .iter()
        .any(|seen| seen.lines.iter().any(|line| hint.is_match(line)))
    {
        return Verdict {
            delivery: Delivery::Queued,
            evidence: "the agent shows a queued-message hint: it will take the input when its \
                       current turn ends"
                .to_string(),
        };
    }
    if before == Some("running") {
        return Verdict {
            delivery: Delivery::Queued,
            evidence: "a turn was already running when it was sent; agent CLIs queue input behind \
                       the running turn"
                .to_string(),
        };
    }
    for seen in after {
        if seen.activity.as_deref() == Some("running") {
            return Verdict {
                delivery: Delivery::Delivered,
                evidence: "the agent started a turn after it".to_string(),
            };
        }
        if seen.activity.as_deref() == Some("awaiting-approval") {
            return Verdict {
                delivery: Delivery::Delivered,
                evidence: "the agent took it and is now asking for approval".to_string(),
            };
        }
    }
    Verdict {
        delivery: Delivery::Unconfirmed,
        evidence: "sent, but no turn started and no queued hint showed while Vogt watched; read \
                   session_screen"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(activity: &str, line: &str) -> Observation {
        Observation {
            activity: (!activity.is_empty()).then(|| activity.to_string()),
            lines: vec![line.to_string()],
        }
    }

    #[test]
    fn the_four_verdicts() {
        assert_eq!(judge(false, None, &[]).delivery, Delivery::Typed);
        assert_eq!(
            judge(
                true,
                Some("idle"),
                &[seen("", "Press up to edit queued messages")]
            )
            .delivery,
            Delivery::Queued
        );
        assert_eq!(judge(true, Some("running"), &[]).delivery, Delivery::Queued);
        assert_eq!(
            judge(true, Some("idle"), &[seen("running", "")]).delivery,
            Delivery::Delivered
        );
        assert_eq!(
            judge(true, Some("idle"), &[seen("awaiting-approval", "")]).delivery,
            Delivery::Delivered
        );
        assert_eq!(
            judge(true, Some("idle"), &[]).delivery,
            Delivery::Unconfirmed
        );
    }
}
