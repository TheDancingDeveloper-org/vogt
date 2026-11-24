// Rendering an initiative as a forge tracking issue. Ports
// `core/initiative_projection.py`. Vogt owns exactly the span between the two
// markers; everything a person writes outside it survives every re-render.

pub const MANAGED_START: &str = "<!-- vogt:initiative:start -->";
pub const MANAGED_END: &str = "<!-- vogt:initiative:end -->";

/// What the task list says when an initiative has no forge-numbered members yet.
pub const EMPTY_TASK_LIST: &str = "_No linked work items yet._";

pub fn marker_for(slug: &str) -> String {
    format!("<!-- vogt:initiative:{slug} -->")
}

/// Python's `str.isspace`: Unicode whitespace plus the C0 controls `\x1c`–`\x1f`.
fn py_space(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{1f}')
}

fn py_strip(text: &str) -> String {
    text.trim_matches(py_space).to_string()
}

fn py_rstrip(text: &str) -> String {
    text.trim_end_matches(py_space).to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLine {
    pub number: i64,
    pub title: String,
    /// True when the member is in a terminal workflow state.
    pub checked: bool,
}

impl TaskLine {
    pub fn from_state(number: i64, title: &str, state: &str) -> Self {
        Self {
            number,
            title: title.to_string(),
            checked: crate::core::TERMINAL_STATES.contains(&state),
        }
    }
}

pub fn render_task_line(line: &TaskLine) -> String {
    let mark = if line.checked { "x" } else { " " };
    py_rstrip(&format!("- [{mark}] #{} {}", line.number, line.title))
}

pub fn render_task_list(lines: &[TaskLine]) -> String {
    if lines.is_empty() {
        return EMPTY_TASK_LIST.to_string();
    }
    lines
        .iter()
        .map(render_task_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_managed_region(
    slug: &str,
    body: &str,
    tasks: &[TaskLine],
    siblings: &[(&str, &str)],
) -> String {
    let mut parts = vec![MANAGED_START.to_string(), marker_for(slug), String::new()];
    let trimmed = py_strip(body);
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
        parts.push(String::new());
    }
    parts.push("### Work items".to_string());
    parts.push(String::new());
    parts.push(render_task_list(tasks));
    if !siblings.is_empty() {
        parts.push(String::new());
        parts.push("### Tracked across other repositories".to_string());
        parts.push(String::new());
        for (sib_slug, url) in siblings {
            parts.push(format!("- {sib_slug}: {url}"));
        }
    }
    parts.push(String::new());
    parts.push(MANAGED_END.to_string());
    parts.join("\n")
}

/// Replace the managed region, preserving everything a person wrote outside it.
/// With no markers, the region is appended.
pub fn splice_managed_region(existing: Option<&str>, region: &str) -> String {
    let Some(existing) =
        existing.filter(|text| text.contains(MANAGED_START) && text.contains(MANAGED_END))
    else {
        let base = py_rstrip(existing.unwrap_or(""));
        return if base.is_empty() {
            region.to_string()
        } else {
            format!("{base}\n\n{region}")
        };
    };
    let start = existing.find(MANAGED_START).unwrap_or(0);
    let end = existing.find(MANAGED_END).unwrap_or(existing.len()) + MANAGED_END.len();
    format!("{}{region}{}", &existing[..start], &existing[end..])
}

pub fn body_has_marker(body: Option<&str>, slug: &str) -> bool {
    body.is_some_and(|text| text.contains(&marker_for(slug)))
}

fn managed_span(body: &str) -> &str {
    let Some(start) = body.find(MANAGED_START) else {
        return "";
    };
    let Some(end) = body.find(MANAGED_END) else {
        return "";
    };
    // A body anyone can edit may put the end marker above the start. Python's
    // slice is empty then; slicing here would panic and take the sweep with it.
    let inner = start + MANAGED_START.len();
    if end < inner {
        return "";
    }
    &body[inner..end]
}

/// The `#<n> -> checked?` map inside the managed region only, so a checkbox a
/// person wrote in their own prose is never read as a member's state. A number
/// repeated in the region keeps its last reading, in the order first seen.
pub fn parse_checkbox_states(body: &str) -> Vec<(String, bool)> {
    let span = managed_span(body);
    if span.is_empty() {
        return Vec::new();
    }
    let pattern = fancy_regex::Regex::new(&crate::decisions::python_pattern(
        r"(?m)^\s*- \[([ xX])\]\s+#(\d+)\b",
    ))
    .expect("pattern");
    let mut order: Vec<(String, bool)> = Vec::new();
    for found in pattern.captures_iter(span) {
        let found = match found {
            Ok(found) => found,
            // The same non-advancing error as `sub`. A checkbox state is not a
            // secret, so the failure drops the rest of the region rather than
            // redacting it.
            Err(_) => break,
        };
        let number = found.get(2).expect("number").as_str().to_string();
        let checked = found
            .get(1)
            .expect("mark")
            .as_str()
            .eq_ignore_ascii_case("x");
        if let Some(slot) = order.iter_mut().find(|(seen, _)| *seen == number) {
            slot.1 = checked;
        } else {
            order.push((number, checked));
        }
    }
    order
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn a_region_round_trips_and_a_human_checkbox_is_ignored() {
        let tasks = vec![
            TaskLine::from_state(12, "Ship it", "done"),
            TaskLine::from_state(13, "Wait", "open"),
        ];
        let region = render_managed_region(
            "alpha",
            "The plan.",
            &tasks,
            &[("beta", "https://forge/beta")],
        );
        assert!(region.starts_with(MANAGED_START));
        assert!(region.contains(&marker_for("alpha")));
        assert!(region.contains("- [x] #12 Ship it"));
        assert!(region.contains("- [ ] #13 Wait"));
        assert!(region.contains("- beta: https://forge/beta"));

        let spliced = splice_managed_region(Some("A note.\n\n- [x] #99 mine\n"), &region);
        assert!(spliced.starts_with("A note."));
        assert_eq!(
            parse_checkbox_states(&spliced),
            vec![("12".into(), true), ("13".into(), false)]
        );
        assert!(body_has_marker(Some(&spliced), "alpha"));
        assert!(!body_has_marker(Some(&spliced), "beta"));
    }

    #[test]
    fn an_end_marker_above_the_start_is_empty_not_a_crash() {
        let swapped = "<!-- vogt:initiative:end -->x<!-- vogt:initiative:start -->";
        assert!(parse_checkbox_states(swapped).is_empty());
        // The span is still empty when the only end marker sits above the start.
        let body = format!("{swapped}\n- [x] #5 outside\n");
        assert!(parse_checkbox_states(&body).is_empty());
        // A repeated number keeps its last tick, in the order first seen.
        let repeated =
            format!("{MANAGED_START}\n- [ ] #5 a\n- [x] #5 b\n- [ ] #3 c\n{MANAGED_END}");
        assert_eq!(
            parse_checkbox_states(&repeated),
            vec![("5".into(), true), ("3".into(), false)]
        );
        // Only a space or an x ticks a box, and the number needs its boundary.
        let noise = format!("{MANAGED_START}\n- [y] #5\n- [] #5\n- [x]#5\n{MANAGED_END}");
        assert!(parse_checkbox_states(&noise).is_empty());
    }

    #[test]
    fn a_control_character_is_whitespace_the_way_python_strips_it() {
        let line = TaskLine::from_state(1, "\u{1f}", "open");
        assert_eq!(render_task_line(&line), "- [ ] #1");
        let region = render_managed_region("alpha", "\u{1f}", &[], &[]);
        assert!(!region.contains('\u{1f}'));
    }

    #[test]
    fn an_empty_initiative_says_so_and_a_fresh_body_is_appended() {
        let region = render_managed_region("alpha", "  ", &[], &[]);
        assert!(region.contains(EMPTY_TASK_LIST));
        assert!(!region.contains("Tracked across"));
        assert_eq!(splice_managed_region(None, &region), region);
    }
}
