//! What a session's agent is told it is working on. Ports
//! `src/vogt/application/services/_brief.py`, and is checked byte for byte
//! against its output: the goldens under `brief_golden/` are recorded by
//! `scripts/gen_brief_golden.py`.
//!
//! The brief is markdown assembled from what Vogt already knows and nothing
//! else. It says only what is recorded. The session's standing guide (how to
//! record, how to say you are blocked, how to drive another session) is
//! written beside it as a companion, and the brief points at that file.

use std::collections::BTreeMap;

use crate::core::{Comment, Project, Relation, WorkItem};
use crate::errors::VogtError;

/// The deployment's own rules, the same for every project. Said in one line,
/// because copying them in would drift from the file that enforces them.
pub const DEPLOYMENT_POLICY: &str = include_str!("brief_golden/text/deployment_policy.md");

/// Written beside the brief. The directory is already on the agent's path.
pub const COMPANION_SUFFIX: &str = ".guide.md";

/// How a session reaches the others. Said in the companion.
pub const DRIVING_OTHER_SESSIONS: &str =
    include_str!("brief_golden/text/driving_other_sessions.md");

/// How an agent says it needs a person. In every companion.
pub const WHEN_BLOCKED: &str = include_str!("brief_golden/text/when_blocked.md");

/// The autopilot convention, added when `session.start` asks for it.
pub const AUTOPILOT: &str = include_str!("brief_golden/text/autopilot.md");

/// The review profile's hint, added when a start asks for a read-only session.
pub const READ_ONLY: &str = include_str!("brief_golden/text/read_only.md");

/// What a reviewer is for. Said in place of the worker's ranking and thread.
const REVIEW: &str = include_str!("brief_golden/text/review.md");

/// The shape of a reviewer's answer, pinned to a sha.
const REVIEW_VERDICT: &str = include_str!("brief_golden/text/review_verdict.md");

/// One row of a `WhyResult`: the ranking contribution, exactly as the Python
/// `ContributionView` carries it.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ContributionView {
    pub input: String,
    pub detail: String,
    pub value: f64,
    pub weight: f64,
    pub contribution: f64,
}
/// The `work.why` result. `inputs_not_yet_available` names ranking inputs that
/// cannot fire in this build, so the brief can say absent rather than zero.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct WhyResult {
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    pub total: f64,
    pub contributions: Vec<ContributionView>,
    pub inputs_not_yet_available: BTreeMap<String, String>,
}

/// The structured half of a hand-off comment (design C11). Every field is
/// optional: a field left out means "not stated", not "empty on purpose".
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HandoffFields {
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub base: Option<String>,
    pub done: Vec<String>,
    pub remaining: Vec<String>,
    pub touch_points: Vec<String>,
    pub gates_run: Vec<String>,
    pub blockers: Vec<String>,
    pub session: Option<String>,
}

/// What a brief is written from. `comments` are the item's comments newest
/// first, as the store returns them. `related` holds the items this one
/// relates to, by id; a relation whose target is absent reads as its id.
pub struct WorkBrief<'a> {
    pub item: &'a WorkItem,
    pub session_id: &'a str,
    pub ranking: Option<&'a WhyResult>,
    pub project: Option<&'a Project>,
    pub comments: &'a [Comment],
    pub related: &'a BTreeMap<String, WorkItem>,
    /// The charters section (WI-2038), from `charter_section`; empty when no
    /// scope has a charter.
    pub charter: &'a [String],
}

/// A brief and the companion written beside it. The companion is empty only
/// for a brief built by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedBrief {
    pub text: String,
    pub companion: String,
    /// The charters section alone (WI-2038), for an agent CLI that takes
    /// standing instructions apart from its first prompt (Codex
    /// `developer_instructions`). Empty when no scope has a charter.
    pub charter: String,
}

/// The scope's charters, broadest first (WI-2038). `found` is
/// `charters::charters_for`. Recorded text, quoted as written; nothing when
/// none is set, because an empty section would read as a rule.
pub fn charter_section(found: &[(String, crate::core::Charter)]) -> Vec<String> {
    if found.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        "## Charters".to_string(),
        String::new(),
        "Standing instructions for this scope, broadest first. A later one refines an earlier one, and the rest of this brief refines them all.".to_string(),
        String::new(),
    ];
    for (heading, charter) in found {
        lines.extend([
            format!("### {heading} (v{})", charter.version),
            String::new(),
            charter.body.trim().to_string(),
            String::new(),
        ]);
    }
    lines
}

/// The project's agent policy, as what it recorded. Empty for a policy that
/// records nothing, so an empty section cannot read as a rule.
fn policy_section(project: Option<&Project>) -> Vec<String> {
    let Some(project) = project else {
        return Vec::new();
    };
    let policy = &project.agent_policy;
    let mut rows = Vec::new();
    if !policy.forbidden_text.is_empty() {
        let rendered = policy
            .forbidden_text
            .iter()
            .map(|pattern| format!("`{pattern}`"))
            .collect::<Vec<_>>()
            .join(", ");
        rows.push(format!(
            "- **Forbidden text** {rendered} — must not appear in a commit \
             message, author, trailer, pull-request text or branch name."
        ));
    }
    if policy.merge_deploys_prod {
        let branches = if policy.main_branches.is_empty() {
            "`main`".to_string()
        } else {
            policy
                .main_branches
                .iter()
                .map(|branch| format!("`{branch}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        rows.push(format!(
            "- **Merges deploy production.** Merging a pull request onto \
             {branches} ships it. Do not run `gh pr merge` there; a person \
             merges."
        ));
    }
    if !policy.commit_identity.is_empty() {
        rows.push(format!("- **Commit identity** {}", policy.commit_identity));
    }
    if !policy.test_recipe.is_empty() {
        rows.push(format!("- **Tests** {}", policy.test_recipe));
    }
    rows.extend(policy.rules.iter().map(|rule| format!("- {rule}")));
    if rows.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        format!("## Project policy — {}", project.slug),
        String::new(),
    ];
    lines.extend(rows);
    lines.push(String::new());
    lines
}

/// The landing rule as the brief states it (`agent_policy.landing`), with
/// `{n}` filled from the item's number. A project with no landing rule renders
/// nothing.
fn landing_section(project: &Project, reference: &str) -> Result<Vec<String>, VogtError> {
    let Some(landing) = &project.agent_policy.landing else {
        return Ok(Vec::new());
    };
    let landing: LandingFields = serde_json::from_value(landing.clone()).map_err(|error| {
        VogtError::InvalidRequest(format!(
            "project {} has an unreadable landing rule: {error}",
            project.slug
        ))
    })?;
    let number = reference
        .split_once('-')
        .map(|(_, number)| number)
        .filter(|number| !number.is_empty())
        .unwrap_or("{n}");
    let pattern = landing.branch_pattern.replace("{n}", number);
    Ok(vec![
        "## Landing rule for this repo".to_string(),
        String::new(),
        format!("- **Base** `{}`", landing.base_branch),
        format!("- **Branch pattern** `{pattern}`"),
        format!("- **Lands by** {}", landing.worker_lands_by),
        format!(
            "- **Merge by** {} ({})",
            landing.merge_by, landing.merge_mode
        ),
        format!("- **Deploy by** {}", landing.deploy_by),
        String::new(),
    ])
}

/// The landing rule, as the project stores it. Read whole, and matching the
/// Python `LandingPolicy` the project write validates.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LandingFields {
    base_branch: String,
    #[serde(default = "default_branch_pattern")]
    branch_pattern: String,
    worker_lands_by: String,
    merge_by: String,
    merge_mode: String,
    #[serde(default = "default_linear_history")]
    #[allow(dead_code)]
    linear_history: bool,
    #[serde(default = "default_deploy_by")]
    deploy_by: String,
}

fn default_branch_pattern() -> String {
    "^wi-{n}-[a-z0-9-]+$".to_string()
}

fn default_linear_history() -> bool {
    true
}

fn default_deploy_by() -> String {
    "operator".to_string()
}

/// The companion: the standing guide written beside a brief. `work_ref` is
/// the item the session is bound to, when there is one.
pub fn companion(session_id: &str, work_ref: Option<&str>) -> String {
    let bound = match work_ref {
        Some(reference) => format!(
            "This session is bound to {reference} (`VOGT_WORK_ITEM`). If you move \
             on to a different item, rebind with `session_bind_work` so the items \
             say who is on them; binding never changes an item's state.\n\n"
        ),
        None => String::new(),
    };
    format!(
        "# Session guide\n\
         \n\
         ## Recording what you find\n\
         \n\
         This session is `{session_id}` and holds a token bound to its own \
         actor, so anything it writes to Vogt is attributed to this session \
         rather than to whoever started it.\n\
         \n\
         Vogt is reachable over MCP at the URL in `VOGT_URL`, with the token \
         in `VOGT_HTTP_TOKEN`. The token may read, and may write work items \
         and comments — nothing else. Every write needs a reason you have \
         actually got: it is stored, and it is what somebody reads later when \
         they ask why this changed.\n\
         \n\
         {bound}{WHEN_BLOCKED}\n{DRIVING_OTHER_SESSIONS}"
    )
}

/// The pointer at the top of the brief's last section, naming the companion.
fn pointer(session_id: &str, work_ref: Option<&str>) -> Vec<String> {
    let bound = match work_ref {
        Some(reference) => {
            format!(" It is bound to {reference}; rebind with `session_bind_work` if you move on.")
        }
        None => String::new(),
    };
    vec![
        "## Before you start".to_string(),
        String::new(),
        format!(
            "This session is `{session_id}`.{bound} How to record what you find, \
             how to say you are blocked, and how to drive another session are in \
             the guide beside this brief: the same file name, ending \
             `{COMPANION_SUFFIX}` instead of `.md`."
        ),
        String::new(),
    ]
}

/// The newest hand-off comment, or none. `comments` is newest first.
fn latest_handoff(comments: &[Comment]) -> Option<&Comment> {
    comments.iter().find(|comment| comment.kind == "handoff")
}

/// A hand-off comment's structured fields, when it carries any that parse.
fn handoff_fields(comment: &Comment) -> Option<HandoffFields> {
    comment
        .handoff
        .as_ref()
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

/// Where the hand-off says its work is, one bullet a line, blanks dropped.
fn handoff_where(fields: &HandoffFields) -> Vec<String> {
    [
        ("branch", fields.branch.as_deref()),
        ("head", fields.head_sha.as_deref()),
        ("base", fields.base.as_deref()),
        ("session", fields.session.as_deref()),
    ]
    .into_iter()
    .filter_map(|(label, value)| match value {
        Some(value) if !value.is_empty() => Some(format!("- **{label}** {value}")),
        _ => None,
    })
    .collect()
}

/// The hand-off's non-empty lists, in the order the brief states them.
fn handoff_lists(fields: &HandoffFields) -> Vec<(&'static str, &Vec<String>)> {
    [
        ("done", &fields.done),
        ("remaining", &fields.remaining),
        ("touch points", &fields.touch_points),
        ("gates run", &fields.gates_run),
        ("blockers", &fields.blockers),
    ]
    .into_iter()
    .filter(|(_, values)| !values.is_empty())
    .collect()
}

/// The structured half of a hand-off, one bullet a line.
fn handoff_facts(fields: &HandoffFields) -> Vec<String> {
    let mut rows = handoff_where(fields);
    for (label, values) in handoff_lists(fields) {
        rows.push(format!("- **{label}** {}", values.join("; ")));
    }
    rows
}

/// The hand-off's lists, each line marked as the worker's claim.
fn handoff_claims(fields: &HandoffFields) -> Vec<String> {
    let mut rows = Vec::new();
    for (label, values) in handoff_lists(fields) {
        rows.push(format!("- **{label}** (claimed)"));
        rows.extend(
            values
                .iter()
                .map(|value| format!("  - claimed, not verified: {value}")),
        );
    }
    rows
}

/// The concrete diff command, with the hand-off's own base and head.
fn review_diff(fields: &HandoffFields) -> Vec<String> {
    let branch = fields.branch.as_deref().unwrap_or_default();
    let base = fields.base.as_deref().unwrap_or_default();
    let head = fields
        .head_sha
        .as_deref()
        .filter(|sha| !sha.is_empty())
        .unwrap_or(branch);
    let mut lines = vec![
        format!(
            "Review `git diff {base}...{head}` — branch `{branch}` against `{base}` — and nothing older."
        ),
        String::new(),
    ];
    match fields.head_sha.as_deref().filter(|sha| !sha.is_empty()) {
        Some(sha) => lines.extend([
            format!(
                "Check the branch tip first (`git rev-parse {branch}`). If it is not `{sha}`, review `{sha}` and say the branch has moved."
            ),
            String::new(),
        ]),
        None => lines.extend([
            format!(
                "The hand-off names no head sha. Resolve one with `git rev-parse {branch}` before you start, review that sha, and say in the verdict that the hand-off left it out."
            ),
            String::new(),
        ]),
    }
    lines
}

/// What a reviewer needs from the project: its gates and how it is deployed.
/// Not the worker's landing rules, which a read-only review must not follow.
fn review_project(project: Option<&Project>) -> Vec<String> {
    let mut lines = Vec::new();
    let policy = project.map(|project| &project.agent_policy);
    if let Some(policy) = policy.filter(|policy| !policy.test_recipe.is_empty()) {
        lines.extend([
            "## Gates".to_string(),
            String::new(),
            format!("- **Tests** {}", policy.test_recipe),
            String::new(),
        ]);
    }
    lines.extend(["## Deployed setup".to_string(), String::new()]);
    match policy.map(|policy| policy.deployed_setup.trim()) {
        Some(setup) if !setup.is_empty() => {
            lines.extend([setup.to_string(), String::new()]);
        }
        _ => lines.extend([
            "This project records no deployed-setup notes (`agent_policy.deployed_setup`). Say so in the verdict, and do not count an in-process test as covering how the project is deployed.".to_string(),
            String::new(),
        ]),
    }
    lines
}

/// The work item, as a page an agent can read before it starts.
///
/// `ranking` is optional because the brief must survive a ranking that cannot
/// be computed. `project` carries the agent policy; without it the brief says
/// nothing about one.
pub fn brief_for_work_item(input: &WorkBrief<'_>) -> Result<ComposedBrief, VogtError> {
    let item = input.item;
    let mut lines = vec![
        format!("# {} — {}", item.reference, item.title),
        String::new(),
    ];
    let mut facts = vec![
        format!("**Kind** {}", item.kind),
        format!("**State** {}", item.state),
        format!("**Priority** {}", item.priority),
    ];
    if let Some(effort) = item.effort {
        facts.push(format!("**Effort** {effort}"));
    }
    if let Some(slug) = &item.project_slug {
        facts.push(format!("**Project** {slug}"));
    }
    if !item.labels.is_empty() {
        facts.push(format!("**Labels** {}", item.labels.join(", ")));
    }
    lines.push(facts.join(" · "));
    lines.push(String::new());
    lines.extend(input.charter.iter().cloned());

    let body = item.body.trim();
    if !body.is_empty() {
        lines.extend([
            "## Description".to_string(),
            String::new(),
            body.to_string(),
            String::new(),
        ]);
    }

    if !item.relations.is_empty() {
        lines.extend(["## Relations".to_string(), String::new()]);
        for relation in &item.relations {
            lines.push(format!(
                "- {} {}",
                relation_kind(relation),
                relation_label(input.related, relation)
            ));
        }
        lines.push(String::new());
    }

    if let Some(ranking) = input.ranking {
        // The description says what the item is; this says why it is above
        // the others, which is the half an agent cannot reconstruct.
        lines.extend([
            "## Why this is ranked where it is".to_string(),
            String::new(),
            format!("Score {}, from:", g(ranking.total)),
            String::new(),
        ]);
        let mut rows = ranking.contributions.clone();
        rows.sort_by(|one, other| {
            other
                .contribution
                .partial_cmp(&one.contribution)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for row in rows {
            let detail = if row.detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", row.detail)
            };
            lines.push(format!(
                "- **{}** {} ({} x {}){detail}",
                row.input,
                signed(row.contribution),
                g(row.value),
                g(row.weight)
            ));
        }
        if !ranking.inputs_not_yet_available.is_empty() {
            lines.extend([
                String::new(),
                "Not yet collected, so absent rather than zero:".to_string(),
                String::new(),
            ]);
            for (name, note) in &ranking.inputs_not_yet_available {
                lines.push(format!("- {name} — {note}"));
            }
        }
        lines.push(String::new());
    }

    // The latest hand-off is its own section, ahead of the remarks, so a fresh
    // session reads where the last one stopped before the thread.
    let handoff_index = input
        .comments
        .iter()
        .position(|comment| comment.kind == "handoff");
    if let Some(comment) = handoff_index.map(|index| &input.comments[index]) {
        if let Some(fields) = handoff_fields(comment) {
            lines.extend(["## Hand-off".to_string(), String::new()]);
            lines.extend(handoff_facts(&fields));
            lines.extend([
                String::new(),
                comment.body.trim().to_string(),
                String::new(),
            ]);
        }
    }
    // A hand-off supersedes the thread before it, so only the remarks made
    // since the latest one are pasted. `comments` is newest first, so the ones
    // ahead of the hand-off are the later ones.
    let since = match handoff_index {
        Some(index) => &input.comments[..index],
        None => input.comments,
    };
    let notes: Vec<&Comment> = since
        .iter()
        .filter(|comment| comment.kind == "note")
        .take(20)
        .collect();
    if !notes.is_empty() {
        lines.extend(["## Comments".to_string(), String::new()]);
        for comment in notes {
            lines.push(format!("- {}", comment.body.trim()));
        }
        lines.push(String::new());
    }

    lines.extend(policy_section(input.project));
    if let Some(project) = input.project {
        lines.extend(landing_section(project, &item.reference)?);
    }
    lines.extend([
        "## Deployment policy".to_string(),
        String::new(),
        DEPLOYMENT_POLICY.to_string(),
        String::new(),
    ]);
    lines.extend(pointer(input.session_id, Some(&item.reference)));
    Ok(ComposedBrief {
        text: lines.join("\n"),
        companion: companion(input.session_id, Some(&item.reference)),
        charter: input.charter.join("\n"),
    })
}

/// A reviewer's brief: the diff against the item's own "done means". Not the
/// worker brief: a reviewer is told what the change has to satisfy and where
/// the change is, and nothing that would start it working.
///
/// With no hand-off there is no branch to diff, so the brief says the diff was
/// not named rather than inventing `main`.
pub fn review_brief_for_work_item(input: &WorkBrief<'_>) -> ComposedBrief {
    let item = input.item;
    let mut lines = vec![
        format!("# Review {} — {}", item.reference, item.title),
        String::new(),
    ];
    let mut facts = vec![
        format!("**Kind** {}", item.kind),
        format!("**State** {}", item.state),
        format!("**Priority** {}", item.priority),
    ];
    if let Some(slug) = &item.project_slug {
        facts.push(format!("**Project** {slug}"));
    }
    lines.extend([facts.join(" · "), String::new(), REVIEW.to_string()]);
    lines.extend(input.charter.iter().cloned());

    lines.extend(["## Done means".to_string(), String::new()]);
    let body = item.body.trim();
    if body.is_empty() {
        lines.extend([
            "This item records no description, so it states no acceptance criteria. Say so in the review; do not invent ones.".to_string(),
            String::new(),
        ]);
    } else {
        lines.extend([body.to_string(), String::new()]);
    }

    lines.extend(["## The diff".to_string(), String::new()]);
    let handoff = latest_handoff(input.comments);
    match handoff.and_then(|comment| handoff_fields(comment).map(|fields| (comment, fields))) {
        Some((comment, fields)) => {
            lines.extend(handoff_where(&fields));
            lines.push(String::new());
            let branch = fields.branch.as_deref().filter(|value| !value.is_empty());
            let base = fields.base.as_deref().filter(|value| !value.is_empty());
            if branch.is_some() && base.is_some() {
                lines.extend(review_diff(&fields));
            } else {
                lines.extend([
                    "The hand-off does not name both a branch and a base, so there is no diff to review yet. Report that and stop.".to_string(),
                    String::new(),
                ]);
            }
            let claims = handoff_claims(&fields);
            let claimed = comment.body.trim();
            if !claims.is_empty() || !claimed.is_empty() {
                lines.extend([
                    "## What the worker claims".to_string(),
                    String::new(),
                    "Everything in this section is the worker's hand-off: a claim, not evidence. Check each line against the diff and the gates you run yourself; do not repeat it as a finding.".to_string(),
                    String::new(),
                ]);
                lines.extend(claims.iter().cloned());
                if !claims.is_empty() {
                    lines.push(String::new());
                }
                if !claimed.is_empty() {
                    lines.extend([
                        "In its own words:".to_string(),
                        String::new(),
                        claimed.to_string(),
                        String::new(),
                    ]);
                }
            }
        }
        None => lines.extend([
            "This item has no hand-off, so no branch or base was named and there is no diff to review yet. Report that and stop.".to_string(),
            String::new(),
        ]),
    }

    lines.extend(review_project(input.project));
    lines.push(REVIEW_VERDICT.to_string());
    lines.extend([
        "## Deployment policy".to_string(),
        String::new(),
        DEPLOYMENT_POLICY.to_string(),
        String::new(),
    ]);
    lines.extend(pointer(input.session_id, Some(&item.reference)));
    ComposedBrief {
        text: lines.join("\n"),
        companion: companion(input.session_id, Some(&item.reference)),
        charter: input.charter.join("\n"),
    }
}

/// A terminal opened on a project, which is a plain shell with context. Thinner
/// than the work-item brief: nobody asked for anything to be done here, so the
/// brief invents no task. It still states the project's policy.
pub fn brief_for_project(
    project_slug: &str,
    session_id: &str,
    project: Option<&Project>,
    charter: &[String],
) -> ComposedBrief {
    let mut lines = vec![
        format!("# {project_slug}"),
        String::new(),
        "A terminal opened on this project. No work item is attached, so there is no task here beyond what you were asked for directly — when you take one up, call `session_bind_work` with its ref (and no `id`), so the item shows who is on it.".to_string(),
        String::new(),
    ];
    lines.extend(charter.iter().cloned());
    lines.extend(policy_section(project));
    lines.extend([
        "## Deployment policy".to_string(),
        String::new(),
        DEPLOYMENT_POLICY.to_string(),
        String::new(),
    ]);
    lines.extend(pointer(session_id, None));
    ComposedBrief {
        text: lines.join("\n"),
        companion: companion(session_id, None),
        charter: charter.join("\n"),
    }
}

/// Python's `relation.kind.replace('_', ' ')`.
fn relation_kind(relation: &Relation) -> String {
    relation.kind.to_string().replace('_', " ")
}

/// A relation to an item that has been deleted still says something; showing
/// the id beats dropping the row silently.
fn relation_label(related: &BTreeMap<String, WorkItem>, relation: &Relation) -> String {
    match related.get(&relation.related_id) {
        None => relation.related_id.clone(),
        Some(item) => format!("{} — {}", item.reference, item.title),
    }
}

/// Python's `+g`: the sign is kept, including on zero and on `nan`.
fn signed(number: f64) -> String {
    let text = g(number);
    if text.starts_with('-') {
        text
    } else {
        format!("+{text}")
    }
}

/// Python's `format(n, "g")`. Rust's `e` format already rounds the way Python
/// does (half to even, on the exact binary value), so this only applies the
/// `%g` presentation rules on top of it: six significant digits, the fixed
/// form when the exponent is between -4 and 5, a two-digit signed exponent
/// otherwise, and trailing zeros dropped.
fn g(number: f64) -> String {
    if number.is_nan() {
        return "nan".to_string();
    }
    if number.is_infinite() {
        return if number.is_sign_negative() {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    let negative = number.is_sign_negative();
    let magnitude = number.abs();
    // Precision 5 is the digits after the leading one, so six in all. The
    // exponent here is decimal and already accounts for a carry from rounding.
    let rendered = format!("{magnitude:.5e}");
    let (mantissa, exponent) = rendered.split_once('e').expect("e format has an exponent");
    let exponent: i32 = exponent.parse().expect("the exponent is a number");
    let digits: String = mantissa.chars().filter(|char| *char != '.').collect();
    let trimmed = digits.trim_end_matches('0');
    let body = if trimmed.is_empty() { "0" } else { trimmed };
    let text = if (-4..6).contains(&exponent) {
        if exponent >= 0 {
            let whole = body.len() as i32 - 1;
            if whole <= exponent {
                format!("{body}{}", "0".repeat((exponent - whole) as usize))
            } else {
                let at = exponent as usize + 1;
                format!("{}.{}", &body[..at], &body[at..])
            }
        } else {
            format!("0.{}{body}", "0".repeat((-exponent - 1) as usize))
        }
    } else {
        let rest = if body.len() > 1 {
            format!(".{}", &body[1..])
        } else {
            String::new()
        };
        format!("{}{rest}e{exponent:+03}", &body[..1])
    };
    if negative {
        format!("-{text}")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        AgentPolicy, Effort, Moment, Origin, Priority, Project, RelationKind, TrustState, WorkKind,
    };
    use serde_json::json;
    use serde_json::Value;

    fn moment() -> Moment {
        Moment::from_unix(0, 0)
    }

    /// The scenarios `scripts/gen_brief_golden.py` records. Keep the two in
    /// step: a change to one is a change to the goldens.
    fn item(id: &str, reference: &str, title: &str) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            reference: reference.to_string(),
            kind: WorkKind::Feature,
            title: title.to_string(),
            body: String::new(),
            state: "in_progress".to_string(),
            priority: Priority::P1,
            effort: None,
            project_id: Some("prj_vogt".to_string()),
            project_slug: Some("vogt".to_string()),
            initiative_id: None,
            origin: Origin::Created,
            trust_state: TrustState::Unverified,
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: moment(),
            updated_at: moment(),
        }
    }

    fn item_full() -> WorkItem {
        WorkItem {
            body: "Port the brief.\n\nKeep it exact.".to_string(),
            effort: Some(Effort::L),
            labels: vec!["rust".to_string(), "brief".to_string()],
            relations: vec![
                Relation {
                    kind: RelationKind::DependsOn,
                    related_id: "wrk_1053".to_string(),
                    related_ref: "WI-1053".to_string(),
                    related_title: "Pure domain III".to_string(),
                    related_state: "done".to_string(),
                },
                Relation {
                    kind: RelationKind::RelatesTo,
                    related_id: "wrk_gone".to_string(),
                    related_ref: "WI-9".to_string(),
                    related_title: "Gone".to_string(),
                    related_state: "open".to_string(),
                },
            ],
            ..item("wrk_1058", "WI-1058", "Port the brief")
        }
    }

    fn plain() -> WorkItem {
        WorkItem {
            kind: WorkKind::Bug,
            state: "open".to_string(),
            ..item("wrk_2", "WI-2", "Plain")
        }
    }

    fn comment(id: &str, body: &str, kind: &str, handoff: Option<Value>) -> Comment {
        Comment {
            id: id.to_string(),
            work_item_id: "wrk_1058".to_string(),
            actor_id: "act_1".to_string(),
            actor_display_name: "Session ses_a".to_string(),
            body: body.to_string(),
            created_at: moment(),
            kind: kind.to_string(),
            handoff,
        }
    }

    fn handoff_full() -> Value {
        json!({
            "branch": "wi-1058-brief",
            "head_sha": "abc1234",
            "base": "rust-core",
            "session": "ses_a",
            "done": ["policy section"],
            "remaining": ["review brief"],
            "touch_points": ["brief.rs"],
            "gates_run": ["cargo test"],
            "blockers": ["none yet"],
        })
    }

    fn comments_full() -> Vec<Comment> {
        vec![
            comment("cmt_3", "Remark after the hand-off.", "note", None),
            comment(
                "cmt_2",
                "Policy section ported; review brief next.",
                "handoff",
                Some(handoff_full()),
            ),
            comment("cmt_1", "Remark before the hand-off.", "note", None),
        ]
    }

    fn ranking_full() -> WhyResult {
        let row = |input: &str, detail: &str, value: f64, weight: f64, contribution: f64| {
            ContributionView {
                input: input.to_string(),
                detail: detail.to_string(),
                value,
                weight,
                contribution,
            }
        };
        WhyResult {
            reference: "WI-1058".to_string(),
            title: "Port the brief".to_string(),
            total: 84.4969,
            contributions: vec![
                row("priority", "p1", 1.0, 55.0, 55.0),
                row(
                    "blocking_fan_out",
                    "2 item(s) depend on this one",
                    2.0,
                    8.0,
                    16.0,
                ),
                row(
                    "staleness",
                    "8.0 days since last change",
                    7.994,
                    0.5,
                    3.9969,
                ),
                row("trust_penalty", "trust state unverified", 1.0, -3.0, -3.0),
            ],
            inputs_not_yet_available: BTreeMap::from([(
                "ci_red_boost".to_string(),
                "arrives with the CI check observations it reads".to_string(),
            )]),
        }
    }

    fn policy_full() -> AgentPolicy {
        AgentPolicy {
            forbidden_text: vec!["secret-[0-9]+".to_string()],
            merge_deploys_prod: true,
            main_branches: vec!["main".to_string()],
            commit_identity: "sprooty <sprooty@sprooty.com>".to_string(),
            test_recipe: "cargo test -p vogt-core".to_string(),
            deployed_setup: "Engine and PWA run as separate services.".to_string(),
            rules: vec!["Keep the lockfile.".to_string()],
            landing: Some(json!({
                "base_branch": "rust-core",
                "worker_lands_by": "push_branch",
                "merge_by": "oversight",
                "merge_mode": "rebase",
            })),
            ..AgentPolicy::default()
        }
    }

    fn project_with(policy: AgentPolicy) -> Project {
        Project {
            agent_policy: policy,
            ..Project::new("prj_vogt", "vogt", "Vogt", "/work/vogt", moment())
        }
    }

    fn related_full() -> BTreeMap<String, WorkItem> {
        BTreeMap::from([(
            "wrk_1053".to_string(),
            WorkItem {
                state: "done".to_string(),
                ..item("wrk_1053", "WI-1053", "Pure domain III")
            },
        )])
    }

    const SESSION: &str = "ses_brief01";

    fn assert_golden(brief: &ComposedBrief, name: &str) {
        assert_eq!(
            brief.text,
            golden(name),
            "{name}: the brief drifted from Python"
        );
        assert_eq!(
            brief.companion,
            golden(&format!("{name}.companion")),
            "{name}: the companion drifted from Python"
        );
    }

    fn golden(name: &str) -> &'static str {
        match name {
            "worker_full" => include_str!("brief_golden/worker_full.md"),
            "worker_full.companion" => include_str!("brief_golden/worker_full.companion.md"),
            "worker_plain" => include_str!("brief_golden/worker_plain.md"),
            "worker_plain.companion" => include_str!("brief_golden/worker_plain.companion.md"),
            "review_full" => include_str!("brief_golden/review_full.md"),
            "review_full.companion" => include_str!("brief_golden/review_full.companion.md"),
            "review_none" => include_str!("brief_golden/review_none.md"),
            "review_none.companion" => include_str!("brief_golden/review_none.companion.md"),
            "review_no_base" => include_str!("brief_golden/review_no_base.md"),
            "review_no_base.companion" => include_str!("brief_golden/review_no_base.companion.md"),
            "review_no_head" => include_str!("brief_golden/review_no_head.md"),
            "review_no_head.companion" => include_str!("brief_golden/review_no_head.companion.md"),
            "project_full" => include_str!("brief_golden/project_full.md"),
            "project_full.companion" => include_str!("brief_golden/project_full.companion.md"),
            "project_bare" => include_str!("brief_golden/project_bare.md"),
            "project_bare.companion" => include_str!("brief_golden/project_bare.companion.md"),
            other => panic!("no golden named {other}"),
        }
    }

    #[test]
    fn a_worker_brief_with_every_section_matches_python() {
        let item = item_full();
        let comments = comments_full();
        let related = related_full();
        let project = project_with(policy_full());
        let ranking = ranking_full();
        let brief = brief_for_work_item(&WorkBrief {
            item: &item,
            session_id: SESSION,
            ranking: Some(&ranking),
            project: Some(&project),
            comments: &comments,
            related: &related,
            charter: &[],
        })
        .unwrap();
        assert_golden(&brief, "worker_full");
    }

    #[test]
    fn a_bare_worker_brief_matches_python() {
        let item = plain();
        let brief = brief_for_work_item(&WorkBrief {
            item: &item,
            session_id: SESSION,
            ranking: None,
            project: None,
            comments: &[],
            related: &BTreeMap::new(),
            charter: &[],
        })
        .unwrap();
        assert_golden(&brief, "worker_plain");
    }

    #[test]
    fn a_review_brief_with_a_full_hand_off_matches_python() {
        let item = item_full();
        let comments = comments_full();
        let related = related_full();
        let project = project_with(policy_full());
        let brief = review_brief_for_work_item(&WorkBrief {
            item: &item,
            session_id: SESSION,
            ranking: None,
            project: Some(&project),
            comments: &comments,
            related: &related,
            charter: &[],
        });
        assert_golden(&brief, "review_full");
    }

    #[test]
    fn a_review_with_no_hand_off_says_there_is_no_diff() {
        let item = plain();
        let brief = review_brief_for_work_item(&WorkBrief {
            item: &item,
            session_id: SESSION,
            ranking: None,
            project: None,
            comments: &[],
            related: &BTreeMap::new(),
            charter: &[],
        });
        assert_golden(&brief, "review_none");
    }

    #[test]
    fn a_review_hand_off_without_a_base_stops_short_of_a_diff() {
        let item = plain();
        let comments = vec![comment(
            "cmt_4",
            "",
            "handoff",
            Some(json!({"branch": "wi-2-x", "done": ["a"], "blockers": ["b"]})),
        )];
        let brief = review_brief_for_work_item(&WorkBrief {
            item: &item,
            session_id: SESSION,
            ranking: None,
            project: None,
            comments: &comments,
            related: &BTreeMap::new(),
            charter: &[],
        });
        assert_golden(&brief, "review_no_base");
    }

    #[test]
    fn a_review_hand_off_without_a_head_sha_resolves_one_from_the_branch() {
        let item = plain();
        let comments = vec![comment(
            "cmt_5",
            "Ported the first half.",
            "handoff",
            Some(json!({"branch": "wi-2-x", "base": "main", "gates_run": ["pytest"]})),
        )];
        let project = project_with(AgentPolicy::default());
        let brief = review_brief_for_work_item(&WorkBrief {
            item: &item,
            session_id: SESSION,
            ranking: None,
            project: Some(&project),
            comments: &comments,
            related: &BTreeMap::new(),
            charter: &[],
        });
        assert_golden(&brief, "review_no_head");
    }

    #[test]
    fn a_project_brief_states_its_policy_and_matches_python() {
        let project = project_with(policy_full());
        assert_golden(
            &brief_for_project("vogt", SESSION, Some(&project), &[]),
            "project_full",
        );
    }

    #[test]
    fn a_project_brief_with_no_policy_matches_python() {
        let project = project_with(AgentPolicy::default());
        assert_golden(
            &brief_for_project("vogt", SESSION, Some(&project), &[]),
            "project_bare",
        );
    }

    #[test]
    fn g_renders_like_python_format_g() {
        assert_eq!(g(84.4969), "84.4969");
        assert_eq!(g(1.0), "1");
        assert_eq!(g(0.0001), "0.0001");
        assert_eq!(g(0.00001), "1e-05");
        assert_eq!(g(1234567.0), "1.23457e+06");
        assert_eq!(signed(-3.0), "-3");
        assert_eq!(signed(0.0), "+0");
    }
}
