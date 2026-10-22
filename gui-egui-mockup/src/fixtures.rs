//! The fake data every screen draws from. Nothing here is read from a server: it is a fixed,
//! realistic snapshot of a Vogt deployment, so the look can be judged without a backend.

/// What a session is doing, as the session rail shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Producing output.
    Running,
    /// Waiting on a person: a question, an approval, a decision.
    NeedsYou,
    /// Idle at its prompt.
    Idle,
    /// Reported itself blocked.
    Blocked,
    /// Hibernated: no process, wakes on input.
    Hibernated,
}

impl SessionState {
    /// Label for badges.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::NeedsYou => "Needs you",
            Self::Idle => "Idle",
            Self::Blocked => "Blocked",
            Self::Hibernated => "Hibernated",
        }
    }
}

/// The role a session plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Oversight: drives workers.
    Oversight,
    /// Worker: one work item.
    Worker,
    /// Reviewer.
    Reviewer,
    /// A shell the operator opened.
    Shell,
}

impl Role {
    /// Label for badges.
    pub fn label(self) -> &'static str {
        match self {
            Self::Oversight => "oversight",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Shell => "shell",
        }
    }
}

/// One agent or shell session.
#[derive(Clone, Debug)]
pub struct Session {
    /// Session id.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Agent CLI.
    pub agent: &'static str,
    /// Model.
    pub model: &'static str,
    /// Role.
    pub role: Role,
    /// State.
    pub state: SessionState,
    /// Bound work item, if any.
    pub work: Option<&'static str>,
    /// Branch.
    pub branch: &'static str,
    /// Time since the last activity.
    pub last_active: &'static str,
    /// Run time.
    pub runtime: &'static str,
    /// Output activity over the last 30 minutes, one value per minute.
    pub activity: Vec<f32>,
    /// Share of the context window used, 0..=1.
    pub context: f32,
    /// Spend so far, in dollars.
    pub cost: f32,
    /// The agent's latest reply, shortened.
    pub excerpt: &'static str,
}

/// A decision a session is waiting on.
#[derive(Clone, Debug)]
pub struct Decision {
    /// Id.
    pub id: &'static str,
    /// The asking session.
    pub session: &'static str,
    /// Work item.
    pub work: &'static str,
    /// The question.
    pub question: &'static str,
    /// Options; the first one is the recommendation.
    pub options: Vec<&'static str>,
    /// How long it has waited.
    pub waiting: &'static str,
}

/// Work-item state, in board order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkState {
    /// Not started.
    Open,
    /// A session is on it.
    InProgress,
    /// Handed off, waiting for review.
    Review,
    /// Waiting on something.
    Blocked,
    /// Finished.
    Done,
}

impl WorkState {
    /// Every state, in board order.
    pub const ALL: [Self; 5] = [
        Self::Open,
        Self::InProgress,
        Self::Review,
        Self::Blocked,
        Self::Done,
    ];

    /// Label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::InProgress => "In progress",
            Self::Review => "Review",
            Self::Blocked => "Blocked",
            Self::Done => "Done",
        }
    }
}

/// One work item.
#[derive(Clone, Debug)]
pub struct WorkItem {
    /// Reference, `WI-…`.
    pub reference: &'static str,
    /// Title.
    pub title: &'static str,
    /// Project.
    pub project: &'static str,
    /// Kind: feature, bug, chore, design, docs.
    pub kind: &'static str,
    /// Priority, `p0`..`p3`.
    pub priority: &'static str,
    /// State.
    pub state: WorkState,
    /// Ranking score.
    pub score: f32,
    /// Assignee (a session name), if any.
    pub assignee: Option<&'static str>,
    /// Time since the last change.
    pub updated: &'static str,
    /// Days since the last change, for sorting.
    pub age_days: f32,
}

/// One comment on a work item.
#[derive(Clone, Debug)]
pub struct Comment {
    /// Author.
    pub author: &'static str,
    /// Kind: note, handoff, review, decision.
    pub kind: &'static str,
    /// When.
    pub when: &'static str,
    /// Markdown body.
    pub body: String,
}

/// Inbox entry kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxKind {
    /// A session asked something.
    Question,
    /// A session handed off.
    Handoff,
    /// CI failed.
    CiFailed,
    /// Drift was detected.
    Drift,
    /// A deploy finished.
    Deploy,
}

/// One inbox notification.
#[derive(Clone, Debug)]
pub struct InboxEntry {
    /// Id.
    pub id: u32,
    /// Kind.
    pub kind: InboxKind,
    /// Title.
    pub title: &'static str,
    /// Body.
    pub body: &'static str,
    /// Where it came from.
    pub source: &'static str,
    /// When.
    pub when: &'static str,
    /// Not read yet.
    pub unread: bool,
}

/// One registered project.
#[derive(Clone, Debug)]
pub struct Project {
    /// Slug.
    pub slug: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Forge repository.
    pub repo: &'static str,
    /// One line on what it is.
    pub summary: &'static str,
    /// Lifecycle.
    pub lifecycle: &'static str,
    /// Open items by state, in board order.
    pub by_state: [f32; 5],
    /// Drift findings.
    pub drift: u32,
    /// Commits per day over two weeks.
    pub commits: Vec<f32>,
    /// Deploy lanes: (lane, version, when).
    pub lanes: Vec<(&'static str, &'static str, &'static str)>,
    /// How changes land.
    pub lands_by: &'static str,
}

/// One changed file in the diff view.
#[derive(Clone, Debug)]
pub struct ChangedFile {
    /// Path.
    pub path: &'static str,
    /// Old text.
    pub old: String,
    /// New text.
    pub new: String,
}

/// The whole fake deployment.
pub struct Fixtures {
    /// Sessions.
    pub sessions: Vec<Session>,
    /// Pending decisions.
    pub decisions: Vec<Decision>,
    /// Work items.
    pub work: Vec<WorkItem>,
    /// Inbox.
    pub inbox: Vec<InboxEntry>,
    /// Projects.
    pub projects: Vec<Project>,
    /// The changed files of one branch.
    pub changes: Vec<ChangedFile>,
}

impl Default for Fixtures {
    fn default() -> Self {
        Self {
            sessions: sessions(),
            decisions: decisions(),
            work: work(),
            inbox: inbox(),
            projects: projects(),
            changes: changes(),
        }
    }
}

impl Fixtures {
    /// The work item with this reference.
    pub fn work_item(&self, reference: &str) -> Option<&WorkItem> {
        self.work.iter().find(|item| item.reference == reference)
    }

    /// The session with this id.
    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|session| session.id == id)
    }
}

/// A deterministic activity trace: `seed` picks the shape, `level` the height.
fn trace(seed: u32, level: f32) -> Vec<f32> {
    (0..30u32)
        .map(|i| {
            let x = (i
                .wrapping_mul(2_654_435_761)
                .wrapping_add(seed.wrapping_mul(40_503))
                >> 16)
                % 100;
            level * (x as f32 / 100.0) * if i > 20 { 1.4 } else { 1.0 }
        })
        .collect()
}

fn sessions() -> Vec<Session> {
    vec![
        Session {
            id: "ses_01M5H2049G",
            name: "oversight",
            agent: "claude",
            model: "opus-5.5",
            role: Role::Oversight,
            state: SessionState::Running,
            work: None,
            branch: "main",
            last_active: "now",
            runtime: "6h 12m",
            activity: trace(1, 9.0),
            context: 0.41,
            cost: 18.40,
            excerpt: "Swept 7 workers. WI-2162 is building the mockup; WI-2150 handed off, review started.",
        },
        Session {
            id: "ses_01M5H2049H",
            name: "wi-2162-fluent-mockup",
            agent: "claude",
            model: "opus-5.5",
            role: Role::Worker,
            state: SessionState::Running,
            work: Some("WI-2162"),
            branch: "wi-2162-vogt-fluent-mockup",
            last_active: "12s ago",
            runtime: "48m",
            activity: trace(2, 12.0),
            context: 0.63,
            cost: 4.92,
            excerpt: "Backlog grid sorts by every column; moving on to the board's drag between columns.",
        },
        Session {
            id: "ses_01M5H1ZZ2K",
            name: "wi-2150-inbox-snooze",
            agent: "codex",
            model: "gpt-5.3-codex",
            role: Role::Worker,
            state: SessionState::NeedsYou,
            work: Some("WI-2150"),
            branch: "wi-2150-inbox-snooze",
            last_active: "4m ago",
            runtime: "1h 05m",
            activity: trace(3, 4.0),
            context: 0.52,
            cost: 2.10,
            excerpt: "Should a snoozed entry come back at its original position or at the top?",
        },
        Session {
            id: "ses_01M5H1XW9P",
            name: "review-wi-2141",
            agent: "claude",
            model: "opus-5.5",
            role: Role::Reviewer,
            state: SessionState::Running,
            work: Some("WI-2141"),
            branch: "wi-2141-terminal-reflow",
            last_active: "40s ago",
            runtime: "22m",
            activity: trace(4, 7.0),
            context: 0.28,
            cost: 1.35,
            excerpt: "Two findings so far: reflow drops the wide-char flag; resize test misses 1-col.",
        },
        Session {
            id: "ses_01M5H1V3QD",
            name: "wi-2139-ci-cache",
            agent: "grok",
            model: "grok-5-fast",
            role: Role::Worker,
            state: SessionState::Blocked,
            work: Some("WI-2139"),
            branch: "wi-2139-ci-cache",
            last_active: "18m ago",
            runtime: "2h 40m",
            activity: trace(5, 2.0),
            context: 0.77,
            cost: 0.88,
            excerpt: "Blocked: the runner has no write access to the sccache bucket (403).",
        },
        Session {
            id: "ses_01M5H1T8AA",
            name: "wi-2133-voice-vad",
            agent: "claude",
            model: "sonnet-5.5",
            role: Role::Worker,
            state: SessionState::Idle,
            work: Some("WI-2133"),
            branch: "wi-2133-voice-vad",
            last_active: "9m ago",
            runtime: "3h 02m",
            activity: trace(6, 1.0),
            context: 0.35,
            cost: 2.71,
            excerpt: "Handed off. Branch pushed at 4be19c0; gates green.",
        },
        Session {
            id: "ses_01M5H1R0ZT",
            name: "node-b shell",
            agent: "bash",
            model: "—",
            role: Role::Shell,
            state: SessionState::Idle,
            work: None,
            branch: "—",
            last_active: "1h ago",
            runtime: "5h 30m",
            activity: trace(7, 0.5),
            context: 0.0,
            cost: 0.0,
            excerpt: "docker compose ps",
        },
        Session {
            id: "ses_01M5H0P4MC",
            name: "wi-2120-history-export",
            agent: "claude",
            model: "haiku-5.5",
            role: Role::Worker,
            state: SessionState::Hibernated,
            work: Some("WI-2120"),
            branch: "wi-2120-history-export",
            last_active: "yesterday",
            runtime: "52m",
            activity: vec![0.0; 30],
            context: 0.19,
            cost: 0.42,
            excerpt: "Hibernated after hand-off.",
        },
        Session {
            id: "ses_01M5H0J7EB",
            name: "wi-2118-drift-badges",
            agent: "codex",
            model: "gpt-5.3-codex",
            role: Role::Worker,
            state: SessionState::Hibernated,
            work: Some("WI-2118"),
            branch: "wi-2118-drift-badges",
            last_active: "2d ago",
            runtime: "1h 17m",
            activity: vec![0.0; 30],
            context: 0.44,
            cost: 1.02,
            excerpt: "Hibernated after hand-off.",
        },
    ]
}

fn decisions() -> Vec<Decision> {
    vec![
        Decision {
            id: "dec-41",
            session: "wi-2150-inbox-snooze",
            work: "WI-2150",
            question: "When a snoozed inbox entry wakes, where should it appear?",
            options: vec![
                "At the top, marked unread",
                "Back at its original position",
                "In a 'Woke up' group",
            ],
            waiting: "4m",
        },
        Decision {
            id: "dec-40",
            session: "wi-2139-ci-cache",
            work: "WI-2139",
            question: "The CI runner cannot write the sccache bucket. Grant the runner role write access, or fall back to rust-cache?",
            options: vec![
                "Grant write on the bucket",
                "Fall back to Swatinem/rust-cache",
            ],
            waiting: "18m",
        },
    ]
}

#[allow(clippy::too_many_arguments)]
fn item(
    reference: &'static str,
    title: &'static str,
    project: &'static str,
    kind: &'static str,
    priority: &'static str,
    state: WorkState,
    score: f32,
    assignee: Option<&'static str>,
    updated: &'static str,
    age_days: f32,
) -> WorkItem {
    WorkItem {
        reference,
        title,
        project,
        kind,
        priority,
        state,
        score,
        assignee,
        updated,
        age_days,
    }
}

fn work() -> Vec<WorkItem> {
    use WorkState::*;
    vec![
        item(
            "WI-2162",
            "Static fluent2-egui mockup of the full Vogt GUI",
            "vogt",
            "feature",
            "p2",
            InProgress,
            22.0,
            Some("wi-2162-fluent-mockup"),
            "12s ago",
            0.0,
        ),
        item(
            "WI-2150",
            "Inbox: snooze entries until a time or an event",
            "vogt",
            "feature",
            "p1",
            InProgress,
            41.5,
            Some("wi-2150-inbox-snooze"),
            "4m ago",
            0.003,
        ),
        item(
            "WI-2141",
            "Terminal reflow keeps wide characters on resize",
            "vogt",
            "bug",
            "p1",
            Review,
            47.0,
            Some("review-wi-2141"),
            "40s ago",
            0.001,
        ),
        item(
            "WI-2139",
            "CI: shared sccache for the engine workspace",
            "vogt",
            "chore",
            "p1",
            Blocked,
            39.0,
            Some("wi-2139-ci-cache"),
            "18m ago",
            0.01,
        ),
        item(
            "WI-2133",
            "Voice: VAD end-of-turn under 300 ms",
            "vogt",
            "feature",
            "p2",
            Review,
            28.0,
            Some("wi-2133-voice-vad"),
            "9m ago",
            0.006,
        ),
        item(
            "WI-2120",
            "Export session history as Markdown",
            "vogt",
            "feature",
            "p3",
            Done,
            9.0,
            None,
            "yesterday",
            1.0,
        ),
        item(
            "WI-2118",
            "Drift badges on the Projects rail",
            "vogt",
            "feature",
            "p2",
            Done,
            14.0,
            None,
            "2d ago",
            2.0,
        ),
        item(
            "WI-2161",
            "Board: WIP limit per column",
            "vogt",
            "feature",
            "p3",
            Open,
            11.0,
            None,
            "1h ago",
            0.04,
        ),
        item(
            "WI-2160",
            "Oversight: batch decisions into one prompt",
            "vogt",
            "design",
            "p2",
            Open,
            24.5,
            None,
            "2h ago",
            0.08,
        ),
        item(
            "WI-2158",
            "Settings: per-device terminal font size",
            "vogt",
            "feature",
            "p3",
            Open,
            8.0,
            None,
            "3h ago",
            0.12,
        ),
        item(
            "WI-2155",
            "Engine: SSE lag resync drops the last frame",
            "vogt",
            "bug",
            "p0",
            Open,
            61.0,
            None,
            "20m ago",
            0.014,
        ),
        item(
            "WI-2152",
            "Docs: how changes land, per repository",
            "vogt",
            "docs",
            "p3",
            Open,
            6.5,
            None,
            "5h ago",
            0.2,
        ),
        item(
            "WI-2149",
            "fluent2: DataGrid column pinning",
            "fluent2-egui",
            "feature",
            "p2",
            Open,
            19.0,
            None,
            "6h ago",
            0.25,
        ),
        item(
            "WI-2147",
            "fluent2: Board / Kanban component",
            "fluent2-egui",
            "feature",
            "p2",
            Open,
            21.0,
            None,
            "6h ago",
            0.25,
        ),
        item(
            "WI-2146",
            "fluent2: TerminalView ligatures off by default",
            "fluent2-egui",
            "bug",
            "p2",
            InProgress,
            18.0,
            Some("fluent2-wi-2146"),
            "35m ago",
            0.02,
        ),
        item(
            "WI-2144",
            "Cadastre: index the new Hetzner volume",
            "cadastre",
            "chore",
            "p2",
            Open,
            12.0,
            None,
            "1d ago",
            1.0,
        ),
        item(
            "WI-2143",
            "Cadastre: secret rotation reminders",
            "cadastre",
            "feature",
            "p3",
            Open,
            7.0,
            None,
            "1d ago",
            1.1,
        ),
        item(
            "WI-2138",
            "FarmEggs: egg-count export to CSV",
            "farmeggs",
            "feature",
            "p2",
            InProgress,
            17.0,
            Some("farmeggs-export"),
            "50m ago",
            0.03,
        ),
        item(
            "WI-2136",
            "FarmEggs: Android back closes the dialog",
            "farmeggs",
            "bug",
            "p1",
            Open,
            33.0,
            None,
            "4h ago",
            0.17,
        ),
        item(
            "WI-2131",
            "ops: Caddy route for the mockup preview",
            "ops",
            "chore",
            "p3",
            Open,
            5.0,
            None,
            "2d ago",
            2.2,
        ),
        item(
            "WI-2129",
            "ops: Loki retention to 30 days",
            "ops",
            "chore",
            "p2",
            Blocked,
            15.0,
            None,
            "3d ago",
            3.0,
        ),
        item(
            "WI-2125",
            "Vogt: rail section order is per device",
            "vogt",
            "bug",
            "p2",
            Done,
            10.0,
            None,
            "4d ago",
            4.0,
        ),
        item(
            "WI-2117",
            "Vogt: hand-off comment template",
            "vogt",
            "feature",
            "p2",
            Done,
            13.0,
            None,
            "5d ago",
            5.0,
        ),
        item(
            "WI-2110",
            "fluent2: Breadcrumb overflow menu",
            "fluent2-egui",
            "feature",
            "p3",
            Done,
            4.0,
            None,
            "6d ago",
            6.0,
        ),
    ]
}

/// Comments on the work item the detail screen shows.
pub fn comments() -> Vec<Comment> {
    vec![
        Comment {
            author: "oversight",
            kind: "note",
            when: "2h ago",
            body: "Started a worker on this with `worktree=true`. Brief: every main screen, both themes, \
                   native and wasm. Reuse `gui-egui/` from slice 1 where it fits.".into(),
        },
        Comment {
            author: "sprooty",
            kind: "decision",
            when: "1h ago",
            body: "Keep it **static**: no backend, no network. I only want to judge the look. \
                   Screenshots of every screen in light and dark, please.".into(),
        },
        Comment {
            author: "wi-2162-fluent-mockup",
            kind: "note",
            when: "12m ago",
            body: "Shell, login, sessions and oversight are in. The backlog grid sorts on every \
                   column; filters are project, priority, state and kind.\n\n- [x] shell and palette\n- [x] sessions\n- [ ] board drag\n- [ ] screenshots".into(),
        },
    ]
}

/// The brief of the session view's session.
pub const BRIEF: &str = "# WI-2162 — Static fluent2-egui mockup of the full Vogt GUI\n\
\n\
**Kind** feature · **Priority** p2 · **Project** vogt\n\
\n\
## Task\n\
\n\
Build a **static** mockup of the whole Vogt GUI in fluent2-egui, with fake data and no \
backend or network calls.\n\
\n\
- Login, app shell, command palette (`Ctrl+K`) and shortcut overlay (`?`)\n\
- Sessions, Oversight, Backlog, Board, Work item, Git diff, Inbox, Projects, Settings\n\
- Native and wasm (trunk)\n\
\n\
## Checks\n\
\n\
1. `cargo clippy --all-targets -- -D warnings`\n\
2. a wasm build\n\
3. kittest smoke tests\n\
\n\
> Stop once pushed; don't poll CI.\n";

/// The body of the work item the detail screen shows.
pub const WORK_BODY: &str = "The operator asked for this on 2026-10-22: a **static** mockup of the whole \
Vogt GUI in fluent2-egui, with fake data and no backend, so the look can be judged. It is separate \
from the live POC (WI-1806).\n\
\n\
## Scope\n\
\n\
Every main screen of today's web GUI:\n\
\n\
- Sessions list and session view (with a terminal)\n\
- Oversight\n\
- Backlog with filter bar\n\
- Board\n\
- Work-item detail with markdown and comments\n\
- Git diff, Inbox, Projects, Settings and theme, login\n\
\n\
Plus the command palette and the shortcut overlay.\n\
\n\
## Builds\n\
\n\
| Target | Command |\n|:--|:--|\n| native | `cargo run -p vogt-gui-mockup` |\n| wasm | `trunk serve` |\n";

/// A fake Claude Code transcript, as raw terminal output.
pub fn transcript() -> String {
    let mut out = String::new();
    out.push_str("\x1b]0;wi-2162-fluent-mockup\x07");
    out.push_str(
        "\x1b[38;5;208m╭──────────────────────────────────────────────────────────╮\x1b[0m\r\n",
    );
    out.push_str("\x1b[38;5;208m│\x1b[0m \x1b[1m✻ Claude Code\x1b[0m  opus-5.5 · ~/Working/Active/WI-2162       \x1b[38;5;208m│\x1b[0m\r\n");
    out.push_str(
        "\x1b[38;5;208m╰──────────────────────────────────────────────────────────╯\x1b[0m\r\n\r\n",
    );
    out.push_str("\x1b[2m> Vogt started this session with a brief. Read it and carry out the task.\x1b[0m\r\n\r\n");
    out.push_str(
        "\x1b[37m●\x1b[0m I'll read the brief, then survey fluent2-egui and the web GUI.\r\n\r\n",
    );
    out.push_str(
        "\x1b[32m●\x1b[0m \x1b[1mBash\x1b[0m(git fetch origin wi-1806-vogt-gui-slice1)\r\n",
    );
    out.push_str("  ⎿  \x1b[2m * branch  wi-1806-vogt-gui-slice1 -> FETCH_HEAD\x1b[0m\r\n\r\n");
    out.push_str("\x1b[32m●\x1b[0m \x1b[1mWrite\x1b[0m(gui-egui-mockup/src/fixtures.rs)\r\n");
    out.push_str("  ⎿  \x1b[2mWrote 612 lines\x1b[0m\r\n\r\n");
    out.push_str("\x1b[32m●\x1b[0m \x1b[1mBash\x1b[0m(CARGO_BUILD_JOBS=4 cargo clippy --all-targets -- -D warnings)\r\n");
    out.push_str("  ⎿  \x1b[2m   Compiling fluent2-egui v0.0.0\x1b[0m\r\n");
    out.push_str("     \x1b[2m   Compiling vogt-gui-mockup v0.1.0\x1b[0m\r\n");
    out.push_str("     \x1b[33mwarning\x1b[0m: unused variable: `row`\r\n");
    out.push_str("       \x1b[34m-->\x1b[0m src/screens/backlog.rs:88:17\r\n\r\n");
    out.push_str("\x1b[37m●\x1b[0m One warning. Fixing the unused binding and re-running.\r\n\r\n");
    out.push_str("\x1b[32m●\x1b[0m \x1b[1mUpdate\x1b[0m(src/screens/backlog.rs)\r\n");
    out.push_str("  ⎿  \x1b[2mUpdated with \x1b[0m\x1b[32m1 addition\x1b[0m\x1b[2m and \x1b[0m\x1b[31m1 removal\x1b[0m\r\n");
    out.push_str(
        "     \x1b[48;5;52m 88 -        .show(ui, &mut self.grid, |row, ui| {   \x1b[0m\r\n",
    );
    out.push_str(
        "     \x1b[48;5;22m 88 +        .show(ui, &mut self.grid, |cell, ui| {  \x1b[0m\r\n\r\n",
    );
    out.push_str("\x1b[32m●\x1b[0m \x1b[1mBash\x1b[0m(cargo test)\r\n");
    out.push_str("  ⎿  \x1b[2mrunning 4 tests\x1b[0m\r\n");
    out.push_str("     test every_screen_renders ... \x1b[32mok\x1b[0m\r\n");
    out.push_str("     test palette_opens_on_ctrl_k ... \x1b[32mok\x1b[0m\r\n");
    out.push_str("     test sign_in_reaches_sessions ... \x1b[32mok\x1b[0m\r\n");
    out.push_str("     test shortcut_overlay_opens ... \x1b[32mok\x1b[0m\r\n");
    out.push_str("     test result: \x1b[32mok\x1b[0m. 4 passed; 0 failed\r\n\r\n");
    out.push_str(
        "\x1b[37m●\x1b[0m Gates are green. Next: the board's drag between columns.\r\n\r\n",
    );
    out.push_str(
        "\x1b[38;5;245m✶ Thinking… (48s · ↓ 2.1k tokens · esc to interrupt)\x1b[0m\r\n\r\n",
    );
    out.push_str(
        "\x1b[38;5;240m╭──────────────────────────────────────────────────────────╮\x1b[0m\r\n",
    );
    out.push_str("\x1b[38;5;240m│\x1b[0m > \x1b[7m \x1b[0m                                                      \x1b[38;5;240m│\x1b[0m\r\n");
    out.push_str(
        "\x1b[38;5;240m╰──────────────────────────────────────────────────────────╯\x1b[0m\r\n",
    );
    out.push_str("  \x1b[2m⏵⏵ auto mode on · 63% context used\x1b[0m");
    out
}

fn inbox() -> Vec<InboxEntry> {
    vec![
        InboxEntry {
            id: 1,
            kind: InboxKind::Question,
            title: "wi-2150-inbox-snooze is asking a question",
            body: "When a snoozed inbox entry wakes, where should it appear?",
            source: "WI-2150 · codex",
            when: "4m",
            unread: true,
        },
        InboxEntry {
            id: 2,
            kind: InboxKind::CiFailed,
            title: "CI failed on wi-2139-ci-cache",
            body: "engine / build: sccache: 403 Forbidden writing to bucket vogt-ci-cache.",
            source: "GitHub Actions · run 39114",
            when: "18m",
            unread: true,
        },
        InboxEntry {
            id: 3,
            kind: InboxKind::Handoff,
            title: "WI-2133 handed off for review",
            body: "Branch wi-2133-voice-vad at 4be19c0. Gates green: clippy, nextest -p voice, vitest.",
            source: "wi-2133-voice-vad",
            when: "9m",
            unread: true,
        },
        InboxEntry {
            id: 4,
            kind: InboxKind::Drift,
            title: "Drift on ops: compose differs from Komodo",
            body: "personal/vogt-dev/compose.yaml has 2 lines Komodo does not deploy.",
            source: "drift sweep",
            when: "1h",
            unread: false,
        },
        InboxEntry {
            id: 5,
            kind: InboxKind::Deploy,
            title: "vogt-dev deployed v0.7.22",
            body: "Pipeline #212 finished in 4m 31s. Receipt committed to vogt-estate.",
            source: "Woodpecker",
            when: "3h",
            unread: false,
        },
        InboxEntry {
            id: 6,
            kind: InboxKind::Handoff,
            title: "WI-2120 handed off for review",
            body: "History export to Markdown; the session hibernated afterwards.",
            source: "wi-2120-history-export",
            when: "yesterday",
            unread: false,
        },
    ]
}

fn projects() -> Vec<Project> {
    vec![
        Project {
            slug: "vogt",
            name: "Vogt",
            repo: "TheDancingDeveloper-org/vogt",
            summary: "The work tracker and agent-session engine this GUI fronts.",
            lifecycle: "active",
            by_state: [6.0, 3.0, 2.0, 1.0, 4.0],
            drift: 0,
            commits: vec![
                4.0, 7.0, 3.0, 9.0, 12.0, 5.0, 2.0, 8.0, 11.0, 6.0, 9.0, 14.0, 10.0, 7.0,
            ],
            lanes: vec![("dev", "v0.7.22", "3h ago"), ("prod", "v0.7.21", "2d ago")],
            lands_by: "push branch · oversight rebases · operator deploys",
        },
        Project {
            slug: "fluent2-egui",
            name: "fluent2-egui",
            repo: "TheDancingDeveloper-org/fluent2-egui",
            summary: "Fluent 2 themes, controls and Office chrome for egui.",
            lifecycle: "active",
            by_state: [2.0, 1.0, 0.0, 0.0, 1.0],
            drift: 0,
            commits: vec![
                2.0, 3.0, 6.0, 4.0, 1.0, 0.0, 5.0, 7.0, 3.0, 2.0, 6.0, 8.0, 4.0, 3.0,
            ],
            lanes: vec![("crates", "v0.2.0", "5d ago")],
            lands_by: "PR · merge queue · tag",
        },
        Project {
            slug: "cadastre",
            name: "Cadastre",
            repo: "indexarr/cadastre",
            summary: "Estate inventory and secrets index.",
            lifecycle: "maintained",
            by_state: [2.0, 0.0, 0.0, 0.0, 0.0],
            drift: 1,
            commits: vec![
                0.0, 1.0, 0.0, 2.0, 0.0, 0.0, 1.0, 0.0, 3.0, 0.0, 1.0, 0.0, 0.0, 2.0,
            ],
            lanes: vec![("prod", "v1.4.2", "9d ago")],
            lands_by: "PR · land-pr",
        },
        Project {
            slug: "farmeggs",
            name: "FarmEggs",
            repo: "TheDancingDeveloper-org/farmeggs",
            summary: "Paddock and egg-count tracking for small farms (PaddockMuster).",
            lifecycle: "active",
            by_state: [1.0, 1.0, 0.0, 0.0, 0.0],
            drift: 0,
            commits: vec![
                1.0, 0.0, 2.0, 3.0, 1.0, 4.0, 2.0, 0.0, 1.0, 5.0, 3.0, 2.0, 1.0, 4.0,
            ],
            lanes: vec![("beta", "1.9.0", "1d ago"), ("play", "1.8.3", "8d ago")],
            lands_by: "PR · merge queue · Play rollout",
        },
        Project {
            slug: "ops",
            name: "ops",
            repo: "indexarr/ops (Forgejo)",
            summary: "Stack compose files and runbooks Komodo deploys from.",
            lifecycle: "maintained",
            by_state: [1.0, 0.0, 0.0, 1.0, 0.0],
            drift: 2,
            commits: vec![
                3.0, 0.0, 0.0, 1.0, 2.0, 0.0, 0.0, 4.0, 1.0, 0.0, 2.0, 0.0, 1.0, 0.0,
            ],
            lanes: vec![("komodo", "main@5c1e0a2", "1h ago")],
            lands_by: "push to main · Komodo deploys",
        },
    ]
}

fn changes() -> Vec<ChangedFile> {
    vec![
        ChangedFile {
            path: "engine/crates/inbox/src/store.rs",
            old: OLD_STORE.to_owned(),
            new: NEW_STORE.to_owned(),
        },
        ChangedFile {
            path: "web/src/inboxApi.ts",
            old: OLD_ROUTE.to_owned(),
            new: NEW_ROUTE.to_owned(),
        },
        ChangedFile {
            path: "engine/crates/inbox/tests/snooze.rs",
            old: OLD_TEST.to_owned(),
            new: NEW_TEST.to_owned(),
        },
    ]
}

const OLD_STORE: &str = r#"use std::collections::BTreeMap;

/// Inbox entries, newest first.
pub struct Inbox {
    entries: BTreeMap<u64, Entry>,
}

impl Inbox {
    pub fn archive(&mut self, id: u64) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.archived = true;
        }
    }

    pub fn active(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values().rev().filter(|e| !e.archived)
    }
}
"#;

const NEW_STORE: &str = r#"use std::collections::BTreeMap;

use crate::clock::Instant;

/// Inbox entries, newest first.
pub struct Inbox {
    entries: BTreeMap<u64, Entry>,
}

impl Inbox {
    pub fn archive(&mut self, id: u64) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.archived = true;
            entry.snoozed_until = None;
        }
    }

    /// Hide `id` until `until`; it comes back at the top, unread.
    pub fn snooze(&mut self, id: u64, until: Instant) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.snoozed_until = Some(until);
        }
    }

    pub fn active(&self, now: Instant) -> impl Iterator<Item = &Entry> {
        self.entries
            .values()
            .rev()
            .filter(move |e| !e.archived && e.snoozed_until.is_none_or(|t| t <= now))
    }
}
"#;

const OLD_ROUTE: &str = r#"export async function archive(id: number): Promise<void> {
  await post(`/api/inbox/${id}/archive`);
}
"#;

const NEW_ROUTE: &str = r#"export async function archive(id: number): Promise<void> {
  await post(`/api/inbox/${id}/archive`);
}

export async function snooze(id: number, until: Date): Promise<void> {
  await post(`/api/inbox/${id}/snooze`, { until: until.toISOString() });
}
"#;

const OLD_TEST: &str = r#"#[test]
fn archived_entries_leave_the_active_list() {
    let mut inbox = Inbox::demo();
    inbox.archive(2);
    assert_eq!(inbox.active().count(), 2);
}
"#;

const NEW_TEST: &str = r#"#[test]
fn archived_entries_leave_the_active_list() {
    let mut inbox = Inbox::demo();
    inbox.archive(2);
    assert_eq!(inbox.active(NOW).count(), 2);
}

#[test]
fn snoozed_entries_come_back_when_due() {
    let mut inbox = Inbox::demo();
    inbox.snooze(1, NOW + HOUR);
    assert_eq!(inbox.active(NOW).count(), 2);
    assert_eq!(inbox.active(NOW + HOUR).count(), 3);
}
"#;
