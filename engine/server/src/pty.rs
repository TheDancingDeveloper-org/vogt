use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::{broadcast, watch, Notify};
use uuid::Uuid;
pub use vogt_engine_contract::{SessionSpec, SessionSummary};

use crate::{
    activity::{classify, strip_ansi, ActivityState},
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    history::{ArchiveRecord, SessionHistory},
    scrollback::Scrollback,
};

/// One chunk of PTY output. `pos` is the byte offset (in the lifetime stream)
/// at which `data` begins. Lets re-attaching clients de-duplicate scrollback
/// replay vs live broadcast.
#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub pos: u64,
    pub data: Bytes,
}

/// The one-shot attach hook: the sampled `pos`, the session's PTY writer and
/// a reader of the ring's live `total_written`.
#[cfg(any(test, feature = "integration-test-hook"))]
pub type FrameRenderHook = Box<dyn FnOnce(u64, &mut dyn Write, &dyn Fn() -> u64) + Send>;

#[cfg(any(test, feature = "integration-test-hook"))]
pub(crate) fn frame_render_hook_cell() -> &'static Mutex<Option<FrameRenderHook>> {
    static HOOK: std::sync::OnceLock<Mutex<Option<FrameRenderHook>>> = std::sync::OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

/// Arm a one-shot hook on the next grid frame render. Available in unit-test
/// builds and to the integration tests via the `integration-test-hook`
/// feature, which shipped builds never enable.
#[cfg(any(test, feature = "integration-test-hook"))]
pub fn arm_frame_render_hook(hook: FrameRenderHook) {
    *frame_render_hook_cell().lock() = Some(hook);
}

pub struct Session {
    pub id: Uuid,
    pub name: Mutex<String>,
    pub created_at: time::OffsetDateTime,
    activity_changed_at: Mutex<time::OffsetDateTime>,
    ended_at: Mutex<Option<time::OffsetDateTime>>,
    pub cwd: String,
    command: Option<String>,
    idle_after_ms: u64,

    scrollback: Mutex<Scrollback>,
    /// The terminal the PTY output is parsed into, so the screen is read from
    /// a grid that has seen every byte rather than replayed from a tail of the
    /// ring (WI-990). Fed by the reader while the `scrollback` lock is held,
    /// so it never describes bytes past the ring's `total_written`; attach
    /// copies its cells under that same lock and renders the copy after it
    /// drops (WI-2699). `None` for a session
    /// restored from hibernation, which has only the tail it kept and renders
    /// by replaying that.
    terminal: Mutex<Option<crate::screen::Terminal>>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    child: Mutex<Option<Box<dyn portable_pty::Child + Send>>>,
    /// Cached child PID — `child` is taken by the wait task, so SIGKILL needs
    /// its own handle on Unix.
    pid: Option<u32>,

    output_tx: broadcast::Sender<OutputChunk>,
    /// The PTY's size as `(cols, rows)`, published so every attached socket
    /// tells its client when another client resizes the PTY (WI-1089).
    size_tx: watch::Sender<(u16, u16)>,
    spawned_at: Instant,
    last_output: Mutex<Option<Instant>>,
    /// When output last changed what the screen shows, which is what activity
    /// and quiet time are measured from: an identical repaint is not
    /// activity (WI-1090).
    screen_change: Mutex<crate::activity::ChangeClock>,
    activity: Mutex<ActivityState>,
    /// When `activity` last changed. Used by the idle-stall watcher to tell
    /// "just went idle" apart from "has been idle for a long time".
    activity_since: Mutex<Instant>,
    activity_epoch: AtomicU64,
    activity_notify: Notify,
    exit_code: Mutex<Option<i32>>,
    history_log_path: Option<PathBuf>,
    reader_done: AtomicBool,
    archive_started: AtomicBool,
    /// Wall-clock start of the current turn (see
    /// `SessionSummary::turn_started_at`).
    turn_started_at: Mutex<Option<time::OffsetDateTime>>,
    /// Wall-clock time of the last PTY output.
    last_output_at: Mutex<Option<time::OffsetDateTime>>,
    /// The permission dialog on screen, while `awaiting-approval`.
    approval: Mutex<Option<crate::approval::Pending>>,
    /// The agent's own report that it is blocked on a person.
    blocked: Mutex<Option<vogt_engine_contract::BlockedReport>>,
    /// How far the standing blocked report has been addressed (WI-1314):
    /// one of the `UNBLOCK_*` steps. A new report resets it.
    unblock: std::sync::atomic::AtomicU8,
    /// A turn has begun since the blocked report was set, so an
    /// `AUTOPILOT: DONE` at the end of a turn is new, not the reporting
    /// reply's own sign-off (WI-1314).
    turn_since_report: AtomicBool,
    /// The agent conversation this session runs, when its id is known
    /// (set by the registry right after spawn).
    conversation: Mutex<Option<vogt_engine_contract::AgentConversation>>,
    /// Pinned awake (see `SessionSummary::keep_awake`).
    keep_awake: AtomicBool,
    /// On autopilot (see `SessionSummary::autopilot`, WI-949).
    autopilot: AtomicBool,
    /// The project's agent policy (WI-1243), when the session carries one.
    /// The policy-check route reads it; a wake restores it from the record.
    policy: Mutex<Option<vogt_engine_contract::SessionPolicy>>,
    /// Times the engine told it to carry on.
    autopilot_nudges: std::sync::atomic::AtomicU32,
    /// Nominated to supervise other sessions (see `SessionSummary::role`,
    /// WI-957).
    oversight: AtomicBool,
    /// Set as the session is being hibernated: the exit that follows is not
    /// the session ending, so its record is kept and its history row says
    /// `hibernated`.
    hibernating: AtomicBool,
    /// When input was last written to the PTY. Every writer moves this,
    /// person or agent: the quiet clock and autopilot read it, and it
    /// cannot tell them apart.
    last_input: Mutex<Option<Instant>>,
    /// When a person last typed (WI-1289). Only the WebSocket path and a
    /// person-gated HTTP `/input` set it. Agent input does not, so it
    /// cannot keep a finished session on the default list.
    last_person_input_at: Mutex<Option<time::OffsetDateTime>>,
    /// The process tree's last resource sample (`resources`).
    resources: Mutex<Option<vogt_engine_contract::SessionResources>>,
    /// The template it was started from, when one was named.
    template: Mutex<Option<String>>,
    /// The work item it serves (see `SessionSummary::work_item`, WI-998).
    work_item: Mutex<Option<String>>,
    /// Its git tree, when it runs in one (WI-1324).
    workspace: Mutex<Option<vogt_engine_contract::Workspace>>,
    /// The session its work was handed to (WI-1251). Kept on the live session
    /// so a later hibernation records the handoff again: the record is
    /// rewritten from the session, and a forced wake must not forget it.
    superseded_by: Mutex<Option<String>>,
    /// The permission posture, when not the default (WI-926).
    permission_mode: Mutex<Option<String>>,
    /// Woken without the bypass it showed before hibernating (WI-2252).
    posture_lost_on_resume: AtomicBool,
    account: Mutex<Option<String>>,
    /// Who asked this session to stop, and why, when someone did (WI-913).
    stop: Mutex<Option<vogt_engine_contract::StopRequest>>,
    /// What runs first in the session: `agent-auth` when the launch wrapper
    /// does (and will report its own stages), `direct` otherwise (WI-927).
    launcher: &'static str,
    /// The first byte of output has been seen and its timing recorded.
    first_output_seen: AtomicBool,
    /// The launch wrapper's report was accepted; a second is refused.
    launch_reported: AtomicBool,
    /// Set when the launch wrapper reported `degraded`: the session started
    /// without its brokered credentials (WI-1812). Lives and dies with it.
    agent_auth_degraded: parking_lot::Mutex<Option<crate::launch::DegradedLaunch>>,
    /// The economics tailer's place in this session's transcript, and the
    /// snapshot it last produced (WI-1245).
    economics: parking_lot::Mutex<Option<crate::economics::Tail>>,
    economics_snapshot: parking_lot::Mutex<Option<vogt_engine_contract::SessionEconomics>>,
    /// The budget the session was started with (WI-1245 / C7).
    budget: Mutex<Option<vogt_engine_contract::SessionBudget>>,
    /// The soft warning has been typed (or is waiting for the prompt). Once,
    /// so a tick does not repeat it.
    budget_soft_sent: AtomicBool,
    /// The hard limit has been acted on. Once, so a wake does not hibernate
    /// the session again for a budget it already met.
    budget_hard_done: AtomicBool,
    /// The overseer has been told this session cannot be hibernated. In memory
    /// only: it is a notice, not a decision that must survive a restart, and it
    /// is deliberately not `budget_soft_sent`, which a soft warning sets first
    /// and which a wall-clock hard can fire before.
    budget_refusal_noted: AtomicBool,
    /// When the budget watcher last saw this session actually running, cleared
    /// whenever it is idle, at its prompt, at a permission dialog, or blocked.
    /// The budget's per-turn wall clock is measured from this, not from
    /// `turn_started_at`: a dialog resumes the same turn, so measuring from the
    /// turn would count the time a person spent answering it.
    budget_running_since: Mutex<Option<time::OffsetDateTime>>,
    /// Running time banked from stretches that have ended, for the
    /// whole-session wall clock (WI-1281). A pause banks the stretch instead of
    /// discarding it. Kept on the hibernation record, so a wake resumes it.
    budget_session_banked: Mutex<Duration>,
    /// A budget action (a soft warning or a hard stop) is due and waits for the
    /// prompt. While set, autopilot does not re-prompt the session, so it comes
    /// to rest at its prompt and the action lands at a turn boundary (WI-1281).
    budget_hold: AtomicBool,
    /// A `ready` change has already been published for the current turn
    /// (WI-1519). Set when one is, cleared when the next turn starts, so a
    /// session that keeps idling does not announce readiness again.
    ready_announced: AtomicBool,
    /// When the agent declared itself done under autopilot this turn (WI-1518),
    /// RFC 3339. Set once per turn, so a re-render of the same screen does not
    /// declare it again; cleared when the next turn starts.
    done_at: Mutex<Option<String>>,
}

impl Session {
    pub fn name(&self) -> String {
        self.name.lock().clone()
    }

    /// `agent-auth` or `direct` (see the field).
    pub fn launcher(&self) -> &'static str {
        self.launcher
    }

    /// Claim the session's one launch report. True the first time only.
    pub fn mark_launch_reported(&self) -> bool {
        !self.launch_reported.swap(true, Ordering::AcqRel)
    }

    /// Why this session started without its brokered credentials, if it did.
    pub fn agent_auth_degraded(&self) -> Option<crate::launch::DegradedLaunch> {
        self.agent_auth_degraded.lock().clone()
    }

    pub fn set_agent_auth_degraded(&self, degraded: crate::launch::DegradedLaunch) {
        *self.agent_auth_degraded.lock() = Some(degraded);
    }

    pub fn activity(&self) -> ActivityState {
        *self.activity.lock()
    }

    /// How long the session has been continuously in its current activity
    /// state.
    pub fn activity_duration(&self) -> std::time::Duration {
        self.activity_since.lock().elapsed()
    }

    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock()
    }

    /// Whether the child process is still running. An exited session stays
    /// in the registry (its scrollback is still readable) until deleted.
    pub fn is_alive(&self) -> bool {
        self.exit_code.lock().is_none()
    }

    /// Parse `bytes` of PTY output into the live grid. A session without one
    /// (restored from hibernation) has nothing to update.
    pub fn feed_terminal(&self, bytes: &[u8]) {
        if let Some(terminal) = self.terminal.lock().as_mut() {
            terminal.process(bytes);
        }
    }

    /// Reflow the live grid to the PTY's new size.
    pub fn resize_terminal(&self, rows: u16, cols: u16) {
        if let Some(terminal) = self.terminal.lock().as_mut() {
            terminal.resize(rows, cols);
        }
    }

    /// The current screen and the scrollback above it. From the live grid
    /// when the session has one — it has parsed every byte since spawn, so a
    /// diff-painting TUI's frame survives past the replay window (WI-990) —
    /// and otherwise by replaying the tail of the ring a hibernated session
    /// kept.
    pub fn render_screen(&self, scrollback_lines: usize) -> (crate::screen::Rendered, Vec<String>) {
        if let Some(terminal) = self.terminal.lock().as_mut() {
            return terminal.render(scrollback_lines);
        }
        let (bytes, rows, cols) = self.screen_source(crate::screen::SCREEN_REPLAY_BYTES);
        crate::screen::render_with_scrollback(&bytes, rows, cols, scrollback_lines)
    }

    /// The bytes and size to render the current screen from: the last
    /// `limit` bytes of the same scrollback ring attach replays (aligned to a
    /// ground-state boundary), and the PTY's current rows and cols.
    pub fn screen_source(&self, limit: usize) -> (Bytes, u16, u16) {
        let (rows, cols) = match self.master.lock().get_size() {
            Ok(size) => (size.rows, size.cols),
            Err(_) => (24, 80),
        };
        let bytes = self.scrollback.lock().snapshot_tail(limit);
        (bytes, rows, cols)
    }

    pub fn command(&self) -> Option<String> {
        self.command.clone()
    }

    /// `created_at`, as the wire spells it.
    pub fn created_at_rfc3339(&self) -> String {
        format_rfc3339(self.created_at)
    }

    /// The child's pid, while the platform reported one.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn conversation(&self) -> Option<vogt_engine_contract::AgentConversation> {
        self.conversation.lock().clone()
    }

    pub fn set_conversation(&self, conversation: Option<vogt_engine_contract::AgentConversation>) {
        *self.conversation.lock() = conversation;
    }

    pub fn keep_awake(&self) -> bool {
        self.keep_awake.load(Ordering::Acquire)
    }

    pub fn set_keep_awake(&self, keep: bool) {
        self.keep_awake.store(keep, Ordering::Release);
    }

    pub fn autopilot(&self) -> bool {
        self.autopilot.load(Ordering::Acquire)
    }

    pub fn set_autopilot(&self, on: bool) {
        self.autopilot.store(on, Ordering::Release);
    }

    /// When the agent declared itself done this turn (WI-1518), RFC 3339.
    pub fn done_at(&self) -> Option<String> {
        self.done_at.lock().clone()
    }

    /// Record that the agent declared itself done this turn (WI-1518). True
    /// the first time; false when this turn already carries the declaration.
    pub fn mark_done(&self, at: String) -> bool {
        let mut done = self.done_at.lock();
        if done.is_some() {
            return false;
        }
        *done = Some(at);
        true
    }

    /// Put a done declaration back onto a session that was just woken, without
    /// announcing it (WI-1518).
    pub fn restore_done_at(&self, at: Option<String>) {
        *self.done_at.lock() = at;
    }

    /// The project policy this session enforces, when it has one (WI-1243).
    pub fn policy(&self) -> Option<vogt_engine_contract::SessionPolicy> {
        self.policy.lock().clone()
    }

    pub fn set_policy(&self, policy: Option<vogt_engine_contract::SessionPolicy>) {
        *self.policy.lock() = policy;
    }

    pub fn budget(&self) -> Option<vogt_engine_contract::SessionBudget> {
        self.budget.lock().clone()
    }

    pub fn set_budget(&self, budget: Option<vogt_engine_contract::SessionBudget>) {
        *self.budget.lock() = budget;
    }

    pub fn budget_soft_sent(&self) -> bool {
        self.budget_soft_sent.load(Ordering::Acquire)
    }

    pub fn set_budget_soft_sent(&self, sent: bool) {
        self.budget_soft_sent.store(sent, Ordering::Release);
    }

    pub fn budget_hard_done(&self) -> bool {
        self.budget_hard_done.load(Ordering::Acquire)
    }

    pub fn set_budget_hard_done(&self, done: bool) {
        self.budget_hard_done.store(done, Ordering::Release);
    }

    pub fn budget_refusal_noted(&self) -> bool {
        self.budget_refusal_noted.load(Ordering::Acquire)
    }

    pub fn set_budget_refusal_noted(&self, noted: bool) {
        self.budget_refusal_noted.store(noted, Ordering::Release);
    }

    pub fn budget_running_since(&self) -> Option<time::OffsetDateTime> {
        *self.budget_running_since.lock()
    }

    /// Remember or forget when the budget's wall clock started counting.
    pub fn set_budget_running_since(&self, since: Option<time::OffsetDateTime>) {
        *self.budget_running_since.lock() = since;
    }

    /// Running time banked from finished stretches.
    pub fn budget_session_banked(&self) -> Duration {
        *self.budget_session_banked.lock()
    }

    pub fn set_budget_session_banked(&self, banked: Duration) {
        *self.budget_session_banked.lock() = banked;
    }

    /// The whole session's running time at `now`: what is banked, plus the
    /// stretch in progress.
    pub fn budget_session_running(&self, now: time::OffsetDateTime) -> Duration {
        let current = self
            .budget_running_since()
            .map(|since| {
                Duration::from_secs(
                    now.unix_timestamp()
                        .saturating_sub(since.unix_timestamp())
                        .max(0) as u64,
                )
            })
            .unwrap_or_default();
        self.budget_session_banked() + current
    }

    pub fn budget_hold(&self) -> bool {
        self.budget_hold.load(Ordering::Acquire)
    }

    pub fn set_budget_hold(&self, hold: bool) {
        self.budget_hold.store(hold, Ordering::Release);
    }

    pub fn role(&self) -> vogt_engine_contract::SessionRole {
        if self.oversight.load(Ordering::Acquire) {
            vogt_engine_contract::SessionRole::Oversight
        } else {
            vogt_engine_contract::SessionRole::Worker
        }
    }

    pub fn set_role(&self, role: vogt_engine_contract::SessionRole) {
        self.oversight.store(
            role == vogt_engine_contract::SessionRole::Oversight,
            Ordering::Release,
        );
    }

    pub fn autopilot_nudges(&self) -> u32 {
        self.autopilot_nudges.load(Ordering::Acquire)
    }

    /// Count one nudge; the new total.
    pub fn count_autopilot_nudge(&self) -> u32 {
        self.autopilot_nudges.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn is_hibernating(&self) -> bool {
        self.hibernating.load(Ordering::Acquire)
    }

    pub fn mark_hibernating(&self) {
        self.hibernating.store(true, Ordering::Release);
    }

    /// Note output that just arrived, settling it against the screen now
    /// unless the screen changed too recently to be worth fingerprinting.
    fn note_output(&self, at: Instant) {
        let interval = std::time::Duration::from_millis(self.idle_after_ms / 4).clamp(
            std::time::Duration::from_millis(50),
            std::time::Duration::from_secs(1),
        );
        let mut clock = self.screen_change.lock();
        if clock.output(at, interval) {
            clock.settle(|| self.terminal.lock().as_ref().map(|t| t.fingerprint()));
        }
    }

    /// When output last changed what the screen shows (see
    /// [`crate::activity::ChangeClock`]), settling any output still pending.
    pub fn last_change(&self) -> Option<Instant> {
        self.screen_change
            .lock()
            .settle(|| self.terminal.lock().as_ref().map(|t| t.fingerprint()))
    }

    /// The window title the program last set, from the live grid.
    pub fn title(&self) -> Option<String> {
        self.terminal.lock().as_ref().and_then(|t| t.title())
    }

    /// The agent CLI the session runs, from its conversation or its command.
    pub fn agent(&self) -> Option<String> {
        if let Some(conversation) = self.conversation() {
            return Some(conversation.agent);
        }
        let argv: Vec<String> = self
            .command
            .as_deref()?
            .split_whitespace()
            .map(str::to_string)
            .collect();
        crate::agent_cli::agent_name(&argv)
    }

    /// How long since any byte crossed the terminal: the last input, the
    /// last output of any kind, or the spawn, whichever is latest. What the
    /// hibernation policy weighs (WI-1090 review): an agent whose mid-turn
    /// animation changes only colours reads `idle` on the change clock, and
    /// a live turn must never be hibernated on that misreading.
    pub fn output_quiet_for(&self) -> std::time::Duration {
        let latest = [*self.last_input.lock(), *self.last_output.lock()]
            .into_iter()
            .flatten()
            .fold(self.spawned_at, |a, b| a.max(b));
        latest.elapsed()
    }

    /// The quiet window before a running session collapses to idle.
    pub fn idle_after(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.idle_after_ms)
    }

    /// How long since anything happened in the terminal: the last input,
    /// the last output that changed the screen, or the spawn, whichever is
    /// latest. What autopilot waits on before it nudges.
    pub fn quiet_for(&self) -> std::time::Duration {
        let latest = [*self.last_input.lock(), self.last_change()]
            .into_iter()
            .flatten()
            .fold(self.spawned_at, |a, b| a.max(b));
        latest.elapsed()
    }

    /// Last `n` bytes of scrollback (or fewer if the buffer holds less).
    pub fn tail(&self, n: usize) -> Bytes {
        Bytes::copy_from_slice(self.scrollback.lock().tail(n))
    }

    pub fn summary(&self) -> SessionSummary {
        // Read once: a guard taken inside the struct literal lives to the end
        // of the statement, so locking `exit_code` again for `alive` there
        // would deadlock (parking_lot mutexes are not re-entrant).
        let exit_code = self.exit_code();
        let sb = self.scrollback.lock();
        SessionSummary {
            id: self.id,
            name: self.name.lock().clone(),
            // An exited child is terminal whatever the cached state says:
            // the exit waiter stores the code a moment before it moves the
            // activity, and a summary read in between said `idle` beside an
            // exit code.
            activity: crate::activity::terminal_state(exit_code, self.stop_requested())
                .unwrap_or_else(|| *self.activity.lock()),
            exit_code,
            alive: exit_code.is_none(),
            scrollback_bytes: sb.total_written(),
            cwd: self.cwd.clone(),
            command: self.command.clone(),
            created_at: format_rfc3339(self.created_at),
            activity_changed_at: format_rfc3339(*self.activity_changed_at.lock()),
            turn_started_at: self.turn_started_at.lock().map(format_rfc3339),
            last_output_at: self.last_output_at.lock().map(format_rfc3339),
            last_event_at: None,
            last_person_input_at: self.last_person_input_at.lock().map(format_rfc3339),
            approval: self.approval_prompt(),
            blocked: self.blocked(),
            conversation: self.conversation(),
            hibernation: None,
            keep_awake: self.keep_awake(),
            autopilot: self.autopilot(),
            autopilot_nudges: self.autopilot_nudges(),
            done_at: self.done_at(),
            role: self.role(),
            work_item: self.work_item(),
            workspace: self.workspace(),
            resources: self.resources.lock().clone(),
            template: self.template.lock().clone(),
            permission_mode: self.permission_mode.lock().clone(),
            posture_lost_on_resume: self.posture_lost_on_resume(),
            account: self.account.lock().clone(),
            stop: self.stop.lock().clone(),
            economics: self.economics_snapshot.lock().clone(),
            budget: self.budget(),
        }
    }

    /// Take the tailer's place so it can read the transcript off the runtime
    /// and hand it back through [`store_economics_tail`].
    pub(crate) fn take_economics_tail(&self) -> Option<crate::economics::Tail> {
        self.economics.lock().take()
    }

    /// Put the tailer's place back, with the snapshot the read produced.
    pub(crate) fn store_economics_tail(
        &self,
        tail: crate::economics::Tail,
        snapshot: Option<vogt_engine_contract::SessionEconomics>,
    ) {
        if snapshot.is_some() {
            *self.economics_snapshot.lock() = snapshot;
        }
        *self.economics.lock() = Some(tail);
    }

    /// The snapshot the tailer last produced, for the exit and hibernate paths.
    pub(crate) fn economics_snapshot(&self) -> Option<vogt_engine_contract::SessionEconomics> {
        self.economics_snapshot.lock().clone()
    }

    /// Record that the session was asked to stop, before anything signals
    /// it, so the exit that follows reads `stopped` (WI-913). The first
    /// request wins: a second kill of a stopping session does not rewrite
    /// who stopped it.
    pub fn request_stop(&self, mut request: vogt_engine_contract::StopRequest) {
        let mut stop = self.stop.lock();
        if stop.is_none() {
            request.at = format_rfc3339(time::OffsetDateTime::now_utc());
            request.reason = request
                .reason
                .map(|r| r.trim().chars().take(500).collect::<String>())
                .filter(|r| !r.is_empty());
            request.by = request
                .by
                .map(|b| b.trim().chars().take(200).collect::<String>())
                .filter(|b| !b.is_empty());
            *stop = Some(request);
        }
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.lock().is_some()
    }

    /// `exited` / `errored` / `stopped`, once the child has exited.
    fn terminal_state(&self) -> Option<ActivityState> {
        crate::activity::terminal_state(self.exit_code(), self.stop_requested())
    }

    pub fn set_permission_mode(&self, mode: Option<String>) {
        *self.permission_mode.lock() = mode;
    }

    pub fn set_posture_lost_on_resume(&self, lost: bool) {
        self.posture_lost_on_resume.store(lost, Ordering::Relaxed);
    }

    pub fn posture_lost_on_resume(&self) -> bool {
        self.posture_lost_on_resume.load(Ordering::Relaxed)
    }

    pub fn set_account(&self, account: Option<String>) {
        *self.account.lock() = account;
    }

    pub fn set_template(&self, template: Option<String>) {
        *self.template.lock() = template;
    }

    pub fn template(&self) -> Option<String> {
        self.template.lock().clone()
    }

    pub fn set_work_item(&self, work_item: Option<String>) {
        *self.work_item.lock() = work_item;
    }

    pub fn work_item(&self) -> Option<String> {
        self.work_item.lock().clone()
    }

    pub fn set_workspace(&self, workspace: Option<vogt_engine_contract::Workspace>) {
        *self.workspace.lock() = workspace;
    }

    pub fn workspace(&self) -> Option<vogt_engine_contract::Workspace> {
        self.workspace.lock().clone()
    }

    pub fn set_superseded_by(&self, superseded_by: Option<String>) {
        *self.superseded_by.lock() = superseded_by;
    }

    pub fn superseded_by(&self) -> Option<String> {
        self.superseded_by.lock().clone()
    }

    pub fn set_resources(&self, resources: Option<vogt_engine_contract::SessionResources>) {
        *self.resources.lock() = resources;
    }

    /// What the session's agent reported it is blocked on, if anything.
    pub fn blocked(&self) -> Option<vogt_engine_contract::BlockedReport> {
        self.blocked.lock().clone()
    }

    /// Put a blocked report back onto a session that was just woken, without
    /// announcing it. `set_blocked` publishes, and a publish becomes a push, so
    /// restoring through it would page the operator again for a block they were
    /// already told about. The next hibernation re-reads whatever is here.
    pub fn restore_blocked(&self, report: Option<vogt_engine_contract::BlockedReport>) {
        *self.blocked.lock() = report;
        self.reset_unblock();
    }

    /// A report starts unaddressed, with no turn run since it was made.
    fn reset_unblock(&self) {
        self.unblock.store(UNBLOCK_NONE, Ordering::Release);
        self.turn_since_report.store(false, Ordering::Release);
    }

    /// Someone other than the engine addressed the standing blocked report:
    /// input over `/input` or a person's keystrokes (`resumes_turn` false),
    /// or an answer to the session's dialog (`resumes_turn` true). The report
    /// clears when the turn that acts on it ends (WI-1314). Input only counts
    /// once a turn has started after it, so input typed while the reporting
    /// turn still runs does not clear the report when that turn ends; an
    /// answer resumes the dialog's own turn, so that turn's end is enough.
    /// The engine's own writes (`write_input` alone) never call this.
    pub fn note_addressed(&self, resumes_turn: bool) {
        if self.blocked.lock().is_none() {
            return;
        }
        let step = if resumes_turn {
            UNBLOCK_ADDRESSING
        } else {
            UNBLOCK_ARMED
        };
        self.unblock.fetch_max(step, Ordering::AcqRel);
    }

    /// Record (or, with `None`, clear) the agent's blocked report and
    /// announce the change. Refused for a session whose child has exited:
    /// nothing is left to unblock.
    pub fn set_blocked(
        &self,
        report: Option<vogt_engine_contract::BlockedReport>,
        bus: &EventBus,
    ) -> Result<()> {
        if !self.is_alive() && report.is_some() {
            return Err(ApiError::Conflict(
                "the session has exited; there is nothing to block".into(),
            ));
        }
        *self.blocked.lock() = report.clone();
        self.reset_unblock();
        bus.publish(ServerEvent::SessionBlocked {
            id: self.id,
            blocked: report,
        });
        Ok(())
    }

    /// The permission dialog on screen, as the wire shows it, with the
    /// countdown computed now.
    pub fn approval_prompt(&self) -> Option<vogt_engine_contract::ApprovalPrompt> {
        self.approval
            .lock()
            .as_ref()
            .map(|p| p.to_wire(time::OffsetDateTime::now_utc()))
    }

    /// The dialog on the screen right now, read fresh from a render rather
    /// than from the last activity pass, so an answer is aimed at what is
    /// actually showing.
    ///
    /// When the rendered screen shows no dialog but the window title says the
    /// agent is awaiting approval, a synthetic permission dialog stands in
    /// (WI-1145, and WI-1279 for Klaudia). It carries no options, so an answer
    /// refuses before any key is written: the guard only needs to know a
    /// person must decide, not which choice to press.
    pub fn current_dialog(&self) -> Option<crate::approval::Detected> {
        let (rendered, scrollback) = self.render_screen(crate::approval::SCROLLBACK_CONTEXT_LINES);
        crate::approval::detect(&rendered.lines, &scrollback)
            .or_else(|| title_approval_dialog(self.title().as_deref()))
    }

    /// When the current turn began.
    pub fn turn_started_at(&self) -> Option<time::OffsetDateTime> {
        *self.turn_started_at.lock()
    }

    /// Snapshot scrollback plus the byte position immediately after the
    /// snapshot. Attached clients use the position to drop already-replayed
    /// broadcast chunks.
    pub fn snapshot(&self) -> (Bytes, u64) {
        let sb = self.scrollback.lock();
        (sb.snapshot(), sb.total_written())
    }

    /// Snapshot at most the last `limit` bytes of scrollback, aligned forward
    /// to a ground-state boundary. The position returned is still the
    /// absolute `total_written`, so a client that later attaches by cursor
    /// resumes without a gap. Copies only the tail while holding the lock,
    /// rather than up to the full ring.
    pub fn snapshot_tail(&self, limit: usize) -> (Bytes, u64) {
        let sb = self.scrollback.lock();
        (sb.snapshot_tail(limit), sb.total_written())
    }

    /// Snapshot only output newer than a client cursor. The boolean tells the
    /// client whether its existing terminal state must be reset.
    ///
    /// `tail_bytes` is the replay **budget** (F1, WI-125), and a live session
    /// answers from its grid rather than the raw ring (WI-121):
    ///
    /// - Warm reattach whose cursor is still retained and whose delta fits the
    ///   budget: the delta, byte-for-byte, with `reset: false`.
    /// - Anything else on a live session — a cold attach, a cursor that aged
    ///   out, or a delta over budget: the grid's frame (`reset: true`), the
    ///   current screen plus its scrollback as escape codes. That is a few KiB
    ///   where the raw path shipped up to the whole budget, and it carries the
    ///   frame a tail replay loses (WI-990).
    /// - A session with no live grid falls back to the bounded raw tail.
    ///
    /// The returned position is always `total_written`, unaffected by trimming
    /// the front, so the live stream resumes with no gap. When `tail_bytes` is
    /// `None` (the in-band resync path, which carries its own cursor) nothing
    /// is bounded and a retained delta stays byte-exact.
    pub fn snapshot_for_attach(
        &self,
        resume_from: Option<u64>,
        tail_bytes: Option<usize>,
    ) -> (Bytes, u64, bool) {
        // The reader thread pushes to the ring and feeds the grid under this
        // lock, so `pos`, the ring tail and the grid all describe one instant
        // while it is held. Reading `pos` and then rendering the live grid
        // under a later acquisition let the reader land output in between:
        // the frame carried bytes past `pos`, the live stream resumed from
        // `pos`, and the client got those bytes twice (WI-891). Rendering
        // under the lock instead stalls the reader for the whole render
        // (WI-2699). So the lock is held only to sample `pos` and copy what
        // the payload is built from — the delta, the raw tail, or a copy of
        // the grid's cells — and the frame is rendered from that copy after
        // the lock drops. The copy describes exactly the bytes up to `pos`.
        // The socket subscribed to the live stream before calling this, and
        // every live chunk carries its own offset, so the stream forwards
        // only bytes at or past `pos` (`ws::coalesce`): that offset handshake
        // makes snapshot and subscribe one boundary, with nothing sent twice
        // and nothing dropped.
        let grid = {
            let sb = self.scrollback.lock();
            let pos = sb.total_written();
            if let Some(cursor) = resume_from {
                if let Some(delta) = sb.snapshot_since(cursor) {
                    if tail_bytes.is_none_or(|limit| delta.len() <= limit) {
                        // A retained delta within budget is byte-exact and
                        // appends (reset:false), so a switch-away that
                        // produced little still catches up without repainting.
                        return (delta, pos, false);
                    }
                }
                // Aged out, or a delta over budget: the live grid's frame,
                // not a tail of the raw ring (WI-121).
            }
            // Lock order stays scrollback, then terminal, as in the reader.
            match self.terminal.lock().as_ref() {
                Some(t) => (t.grid(), pos),
                // A session restored from hibernation has no live grid:
                // replay the raw tail, as a hibernated attach does.
                None => return (raw_tail(&sb, tail_bytes), pos, true),
            }
        };
        let (mut grid, pos) = grid;
        // The attach-boundary hook (WI-2699): `pos` is sampled, the grid
        // copied and the lock dropped, so a test can write to the PTY here,
        // watch the reader land it, and then check the frame still stops at
        // `pos`. Production never arms the hook.
        #[cfg(any(test, feature = "integration-test-hook"))]
        {
            let hook = frame_render_hook_cell().lock().take();
            if let Some(hook) = hook {
                let written = || self.scrollback.lock().total_written();
                hook(pos, &mut **self.writer.lock(), &written);
            }
        }
        let frame = crate::screen::frame_of(&mut grid);
        (self.bounded_frame(frame, tail_bytes), pos, true)
    }

    /// The frame, cut to `tail_bytes` when one was given and the frame is
    /// larger. The cut keeps the tail — the visible screen is emitted last —
    /// and moves forward to a ground-state boundary, so it never starts inside
    /// an escape sequence. A frame is a few KiB in practice; the bound only
    /// bites for a grid holding a deep scrollback or a very small budget.
    fn bounded_frame(&self, frame: Vec<u8>, tail_bytes: Option<usize>) -> Bytes {
        let Some(limit) = tail_bytes else {
            return Bytes::from(frame);
        };
        if frame.len() <= limit {
            return Bytes::from(frame);
        }
        let mut sb = crate::scrollback::Scrollback::new(frame.len());
        sb.push(&frame);
        sb.snapshot_tail(limit)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OutputChunk> {
        self.output_tx.subscribe()
    }

    /// Current absolute output position, used by the WebSocket liveness probe
    /// to detect a socket that is open but no longer delivering output.
    pub fn scrollback_position(&self) -> u64 {
        self.scrollback.lock().total_written()
    }

    /// Whether the program in the PTY has bracketed paste on. False when
    /// the session keeps no grid.
    pub fn bracketed_paste(&self) -> bool {
        self.terminal
            .lock()
            .as_ref()
            .is_some_and(crate::screen::Terminal::bracketed_paste)
    }

    pub fn write_input(&self, data: &[u8]) -> std::io::Result<()> {
        *self.last_input.lock() = Some(Instant::now());
        let mut w = self.writer.lock();
        w.write_all(data)?;
        w.flush()
    }

    /// Stamp that a person typed. Called only by the paths that already
    /// know the caller is a person (the WebSocket, and HTTP `/input` after
    /// `person_gate::is_person`). Agent writes go through `write_input`
    /// alone and leave this clock where it is.
    pub fn note_person_input(&self) {
        *self.last_person_input_at.lock() = Some(time::OffsetDateTime::now_utc());
    }

    pub fn last_person_input_at(&self) -> Option<time::OffsetDateTime> {
        *self.last_person_input_at.lock()
    }

    /// When the current activity began. The rail's hidden filter reads this
    /// instead of building a full summary (WI-2692).
    pub fn activity_changed_at_stamp(&self) -> time::OffsetDateTime {
        *self.activity_changed_at.lock()
    }

    pub fn last_output_at_stamp(&self) -> Option<time::OffsetDateTime> {
        *self.last_output_at.lock()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let master = self.master.lock();
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| ApiError::Pty(format!("resize: {e}")))?;
        drop(master);
        // Reflow the grid to match, so a screen read before the program's own
        // repaint arrives is already the new size.
        self.resize_terminal(rows, cols);
        self.size_tx.send_if_modified(|size| {
            let changed = *size != (cols, rows);
            *size = (cols, rows);
            changed
        });
        Ok(())
    }

    /// Follow the PTY's `(cols, rows)`: the receiver's current value is the
    /// size now, and `changed()` fires on every resize that alters it.
    pub fn watch_size(&self) -> watch::Receiver<(u16, u16)> {
        self.size_tx.subscribe()
    }

    pub fn rename(&self, new_name: String) {
        *self.name.lock() = new_name;
    }

    pub fn kill(&self) -> Result<()> {
        // Prefer signalling by pid: the wait task has typically already taken
        // ownership of the `Child` to call `wait()`, leaving the Mutex empty.
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SIGKILL — same semantics as portable_pty::Child::kill on unix.
            // Ignore ESRCH (child already reaped).
            unsafe {
                let rc = libc::kill(pid as libc::pid_t, libc::SIGKILL);
                if rc != 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::ESRCH) {
                        return Err(ApiError::Pty(format!("kill({pid}): {err}")));
                    }
                }
            }
            return Ok(());
        }
        if let Some(child) = self.child.lock().as_mut() {
            child
                .kill()
                .map_err(|e| ApiError::Pty(format!("kill: {e}")))?;
        }
        Ok(())
    }
}

/// The bounded raw tail of a ring, taken while its lock is held. A session
/// restored without a live grid replays this, exactly as a hibernated attach.
fn raw_tail(sb: &crate::scrollback::Scrollback, tail_bytes: Option<usize>) -> Bytes {
    match tail_bytes {
        Some(limit) => sb.snapshot_tail(limit),
        None => sb.snapshot(),
    }
}

fn format_rfc3339(ts: time::OffsetDateTime) -> String {
    ts.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| ts.to_string())
}

pub struct SpawnedSession {
    pub session: Arc<Session>,
}

pub struct SpawnDefaults<'a> {
    pub default_shell: &'a str,
    pub auto_agent_auth: bool,
    pub agent_auth_helper: &'a Path,
    pub default_cwd: &'a Path,
    pub scrollback_bytes: usize,
    pub activity_idle_after_ms: u64,
    /// The session's credential for the on-demand secret broker, or
    /// `None` when the deployment declares nothing to broker.
    pub secret_broker: Option<crate::secret_broker::BrokerGrant>,
    /// Called once the child has exited and its output is drained, after
    /// `SessionKilled` is published. The registry uses it to forget the
    /// hibernation record of a session that ended (rather than hibernated).
    pub on_exit: Option<ExitHook>,
}

/// See [`SpawnDefaults::on_exit`].
pub type ExitHook = Box<dyn FnOnce(&Session) + Send + 'static>;

fn command_display(spec: &SessionSpec, defaults: &SpawnDefaults<'_>) -> String {
    if let Some(argv) = spec.command.as_ref().filter(|argv| !argv.is_empty()) {
        return argv
            .iter()
            .map(|arg| shell_escape(arg))
            .collect::<Vec<_>>()
            .join(" ");
    }
    if defaults.auto_agent_auth {
        return format!("{} shell", defaults.agent_auth_helper.display());
    }
    defaults.default_shell.to_string()
}

fn shell_escape(arg: &str) -> String {
    if arg.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b':' | b'=' | b'+')
    }) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Whether an environment variable the engine holds is a secret a child
/// process must never inherit.
///
/// A PTY session, GUI process or agent-task run inherits the engine's full
/// process environment by default (`CommandBuilder::new` copies the parent
/// env; `tokio::process::Command` likewise). That environment carries
/// `ENGINE_TOKEN`, `ENGINE_EXTRA_TOKENS_JSON` (every scoped token, plus any
/// paired core tokens), `ENGINE_ASSISTANT_API_KEY`, the STT/TTS keys, the
/// Infisical machine identity and `TAILSCALE_AUTH_KEY`. A caller holding only
/// the `sessions` capability could `echo $ENGINE_EXTRA_TOKENS_JSON` and take
/// every other token; prompt-injected agent content reads them for free. The
/// capability model collapses.
///
/// A denylist rather than an allowlist, so the pod's toolchain, locale and
/// agent configuration survive untouched — only credentials are dropped.
/// Caller-supplied `spec.env` is applied *after* this base and is intentional,
/// so it is never filtered here.
pub(crate) fn is_secret_env(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    k.starts_with("ENGINE_")
        || k.starts_with("INFISICAL_")
        || k == "TAILSCALE_AUTH_KEY"
        || k.contains("TOKEN")
        || k.contains("SECRET")
        || k.contains("PASSWORD")
        || k.contains("API_KEY")
}

/// The engine's own environment with the secrets stripped — the base a child
/// process should start from before its session/task variables are added.
pub(crate) fn sanitized_child_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(k, _)| match k.to_str() {
            Some(s) => !is_secret_env(s),
            // A non-UTF-8 key cannot be one of our (ASCII-named) secrets.
            None => true,
        })
        .collect()
}

/// Whether an environment variable is one the agent-auth helper (and only the
/// helper) is re-granted on top of [`sanitized_child_env`].
///
/// The default sanitisation strips the Infisical machine identity and the `ENGINE_AGENT_AUTH_*`
/// manifest from every child. The trusted helper needs exactly these to broker
/// the agent's tokens, and drops them again before it launches the user's
/// shell (`engine/deploy/agent-auth.sh`). This is a tight allowlist, not an
/// exemption from stripping: the broad engine secrets (`ENGINE_TOKEN`, the
/// scoped token set, the assistant/STT/TTS keys, `TAILSCALE_AUTH_KEY`) are
/// still withheld from the helper.
fn is_agent_auth_helper_env(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    matches!(
        k.as_str(),
        "INFISICAL_API_URL"
            | "INFISICAL_CLIENT_ID"
            | "INFISICAL_CLIENT_SECRET"
            | "ENGINE_INFISICAL_ENV"
    ) || k.starts_with("ENGINE_AGENT_AUTH_")
}

/// The subset of the engine's environment the agent-auth helper is re-granted:
/// the brokering identity and manifest [`is_agent_auth_helper_env`] names, which
/// [`sanitized_child_env`] would otherwise have stripped.
pub(crate) fn agent_auth_helper_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(k, _)| match k.to_str() {
            Some(s) => is_agent_auth_helper_env(s),
            None => false,
        })
        .collect()
}

/// The four Infisical machine-identity variables stripped from every child.
/// In identity-passthrough mode an opted-in deployment re-grants exactly
/// these to sessions so an operator can drive `infisical` directly against any
/// project; the broad engine credentials `is_secret_env` withholds stay stripped.
fn is_identity_env(key: &str) -> bool {
    matches!(
        key.to_ascii_uppercase().as_str(),
        "INFISICAL_API_URL"
            | "INFISICAL_CLIENT_ID"
            | "INFISICAL_CLIENT_SECRET"
            | "ENGINE_INFISICAL_ENV"
    )
}

/// Parse the `ENGINE_AGENT_AUTH_IDENTITY_PASSTHROUGH` switch. Off unless
/// explicitly truthy, so the manifest-brokered model stays the default.
fn identity_passthrough_from(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("on")
    )
}

/// Whether this deployment opts every session into holding the full secrets-
/// manager identity, reversing the default strip for the four identity vars.
pub(crate) fn identity_passthrough_enabled() -> bool {
    identity_passthrough_from(
        std::env::var("ENGINE_AGENT_AUTH_IDENTITY_PASSTHROUGH")
            .ok()
            .as_deref(),
    )
}

/// The Infisical identity variables re-granted to a session in identity-
/// passthrough mode [`identity_passthrough_enabled`], which [`sanitized_child_env`]
/// would otherwise have stripped.
pub(crate) fn identity_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(k, _)| match k.to_str() {
            Some(s) => is_identity_env(s),
            None => false,
        })
        .collect()
}

/// Is the child command's program the configured agent-auth helper? A session
/// launched with an explicit `spec.command` reaches the helper on its own when
/// the "(protected)" templates run `agent-auth run -- <cli>` — and agent
/// frameworks, subagents and worktree sessions launch it the same way. Such a
/// child needs the broker manifest re-granted just like the engine-chosen
/// `<helper> shell` path; without it the child starts with the
/// `ENGINE_AGENT_AUTH_*` variables already stripped by [`is_secret_env`], brokers
/// nothing, and `agent-auth.sh` unsets `VOGT_HTTP_TOKEN` — a session with no
/// Vogt/git/gh credentials. The templates name the helper either by its full
/// configured path or by just its basename on `PATH`, so accept both (#727).
pub(crate) fn is_agent_auth_helper_command(argv0: &str, helper: &Path) -> bool {
    Path::new(argv0) == helper
        || helper
            .file_name()
            .is_some_and(|base| Path::new(argv0) == Path::new(base))
}

/// Spawn a PTY-backed session running `spec.command` (or the default shell if
/// `None`). Starts the reader thread, the exit waiter, and the activity watcher.
///
/// `id` is allocated by the caller: the registry needs it before the spawn so
/// per-session artifacts (the prompt file) can be named for the session and
/// exist by the time the child's first line runs.
pub fn spawn(
    id: Uuid,
    spec: &SessionSpec,
    defaults: SpawnDefaults<'_>,
    bus: EventBus,
    history: Option<Arc<SessionHistory>>,
) -> Result<SpawnedSession> {
    let pty_system = portable_pty::native_pty_system();
    let size = PtySize {
        rows: spec.rows.unwrap_or(24),
        cols: spec.cols.unwrap_or(80),
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = pty_system
        .openpty(size)
        .map_err(|e| ApiError::Pty(format!("openpty: {e}")))?;

    // Whether this spawn is the trusted agent-auth helper itself (rather than a
    // caller command or a bare shell). The helper brokers agent tokens out of
    // Infisical and needs the machine identity that is otherwise stripped; it is
    // re-granted below and drops it again before it execs the user's shell.
    let mut spawning_agent_auth_helper = false;
    let mut cmd = match spec.command.as_ref() {
        Some(argv) if !argv.is_empty() => {
            // A command-launched session whose program IS the agent-auth helper
            // (the "(protected)" templates run `agent-auth run -- <cli>`, and
            // subagents/frameworks/worktree sessions do the same) must broker the
            // same token set as the engine-chosen `shell` path — see
            // is_agent_auth_helper_command (#727). Otherwise the manifest is
            // stripped and the child ends up with no VOGT_HTTP_TOKEN.
            spawning_agent_auth_helper =
                is_agent_auth_helper_command(&argv[0], defaults.agent_auth_helper);
            let mut c = CommandBuilder::new(&argv[0]);
            for a in &argv[1..] {
                c.arg(a);
            }
            c
        }
        _ if defaults.auto_agent_auth => {
            spawning_agent_auth_helper = true;
            let mut c = CommandBuilder::new(defaults.agent_auth_helper);
            c.arg("shell");
            c
        }
        _ => CommandBuilder::new(defaults.default_shell),
    };
    let cwd = spec
        .cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| defaults.default_cwd.to_path_buf());
    let cwd_display = cwd.to_string_lossy().into_owned();
    cmd.cwd(cwd);
    // Start from the engine's environment with the credentials stripped
    // A child session/agent must not inherit ENGINE_TOKEN, the scoped
    // token set, the assistant API key, the Infisical identity or the
    // tailscale key. env_clear() drops the default parent-env copy; we then
    // repopulate the non-secret variables the pod's shells and agents need.
    cmd.env_clear();
    for (k, v) in sanitized_child_env() {
        cmd.env(k, v);
    }
    // The agent-auth helper is the one child that legitimately needs the
    // brokering identity the sanitiser strips: it reads the Infisical machine identity
    // and the ENGINE_AGENT_AUTH_* manifest to mint the agent's git/gh/MCP
    // tokens, then unsets them before it launches the user's shell. Re-grant
    // exactly those variables — and only to the helper command, never to a
    // caller command or a bare shell.
    if spawning_agent_auth_helper {
        for (k, v) in agent_auth_helper_env() {
            cmd.env(k, v);
        }
    }
    // An opted-in deployment (ENGINE_AGENT_AUTH_IDENTITY_PASSTHROUGH) hands
    // every session the full secrets-manager identity, reversing the default strip for
    // exactly the four Infisical identity vars. The agent-auth helper already
    // holds them (above); this also covers a bare shell or a caller command. The
    // broad engine credentials (ENGINE_TOKEN, the scoped token set, assistant/STT
    // keys, TAILSCALE_AUTH_KEY) stay stripped in both modes.
    if identity_passthrough_enabled() {
        for (k, v) in identity_env() {
            cmd.env(k, v);
        }
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("VOGT_ENGINE_SESSION", &spec.name);
    // The session id is allocated before the spawn so the child can be told
    // which session it is. `VOGT_ENGINE_SESSION` is the display name and is not
    // unique, so it cannot be used to identify a session.
    cmd.env("VOGT_ENGINE_SESSION_ID", id.to_string());
    // The broker credential is set here, after the strip, because it is
    // minted for this child and never lives in the engine's own environment
    // (where `is_secret_env` would rightly withhold it). It survives the
    // helper's own `unset` of the brokering identity: it is not the identity,
    // it is the session's leave to ask for one manifest secret at a time.
    if let Some(grant) = defaults.secret_broker.as_ref() {
        cmd.env(crate::secret_broker::BROKER_TOKEN_ENV, &grant.token);
        cmd.env(crate::secret_broker::BROKER_URL_ENV, &grant.url);
    }
    if let Some(env) = spec.env.as_ref() {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| ApiError::Pty(format!("spawn: {e}")))?;
    let pid = child.process_id();
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| ApiError::Pty(format!("clone_reader: {e}")))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| ApiError::Pty(format!("take_writer: {e}")))?;

    let (tx, _rx) = broadcast::channel::<OutputChunk>(1024);
    let command = Some(command_display(spec, &defaults));
    let runtime = tokio::runtime::Handle::current();
    let history_log_path = history.as_ref().map(|h| h.log_path(id));
    let history_log = history.as_ref().and_then(|h| match h.open_log_writer(id) {
        Ok(file) => Some(file),
        Err(e) => {
            tracing::warn!(session = %id, error = %e, "session history log disabled");
            None
        }
    });

    let created_at = time::OffsetDateTime::now_utc();
    let session = Arc::new(Session {
        id,
        name: Mutex::new(spec.name.clone()),
        created_at,
        activity_changed_at: Mutex::new(created_at),
        ended_at: Mutex::new(None),
        cwd: cwd_display,
        command,
        idle_after_ms: defaults.activity_idle_after_ms,
        scrollback: Mutex::new(Scrollback::new(
            spec.scrollback_bytes.unwrap_or(defaults.scrollback_bytes),
        )),
        terminal: Mutex::new(Some(crate::screen::Terminal::new(size.rows, size.cols))),
        writer: Mutex::new(writer),
        master: Mutex::new(pair.master),
        child: Mutex::new(Some(child)),
        pid,
        output_tx: tx.clone(),
        size_tx: watch::Sender::new((size.cols, size.rows)),
        spawned_at: Instant::now(),
        last_output: Mutex::new(None),
        screen_change: Mutex::new(crate::activity::ChangeClock::default()),
        activity: Mutex::new(ActivityState::Running),
        activity_since: Mutex::new(Instant::now()),
        activity_epoch: AtomicU64::new(0),
        activity_notify: Notify::new(),
        exit_code: Mutex::new(None),
        history_log_path,
        reader_done: AtomicBool::new(false),
        archive_started: AtomicBool::new(false),
        // A session starts running, so its first turn starts with it.
        turn_started_at: Mutex::new(Some(created_at)),
        last_output_at: Mutex::new(None),
        approval: Mutex::new(None),
        blocked: Mutex::new(None),
        unblock: std::sync::atomic::AtomicU8::new(UNBLOCK_NONE),
        turn_since_report: AtomicBool::new(false),
        conversation: Mutex::new(None),
        keep_awake: AtomicBool::new(false),
        autopilot: AtomicBool::new(false),
        policy: Mutex::new(None),
        autopilot_nudges: std::sync::atomic::AtomicU32::new(0),
        oversight: AtomicBool::new(false),
        hibernating: AtomicBool::new(false),
        last_input: Mutex::new(None),
        last_person_input_at: Mutex::new(None),
        resources: Mutex::new(None),
        template: Mutex::new(None),
        work_item: Mutex::new(None),
        workspace: Mutex::new(None),
        superseded_by: Mutex::new(None),
        permission_mode: Mutex::new(None),
        posture_lost_on_resume: AtomicBool::new(false),
        account: Mutex::new(None),
        stop: Mutex::new(None),
        launcher: if spawning_agent_auth_helper {
            "agent-auth"
        } else {
            "direct"
        },
        first_output_seen: AtomicBool::new(false),
        launch_reported: AtomicBool::new(false),
        agent_auth_degraded: parking_lot::Mutex::new(None),
        economics: parking_lot::Mutex::new(None),
        economics_snapshot: parking_lot::Mutex::new(None),
        // Set by the registry from the spec, like autopilot: the spawn path
        // does not see the spec's budget.
        budget: Mutex::new(None),
        budget_soft_sent: AtomicBool::new(false),
        budget_hard_done: AtomicBool::new(false),
        budget_refusal_noted: AtomicBool::new(false),
        budget_running_since: Mutex::new(None),
        budget_session_banked: Mutex::new(Duration::ZERO),
        budget_hold: AtomicBool::new(false),
        ready_announced: AtomicBool::new(false),
        done_at: Mutex::new(None),
    });

    spawn_reader_thread(
        Arc::clone(&session),
        reader,
        tx,
        bus.clone(),
        history.clone(),
        history_log,
        runtime.clone(),
    )?;
    spawn_exit_waiter(
        Arc::clone(&session),
        bus.clone(),
        history.clone(),
        runtime,
        defaults.on_exit,
    );
    spawn_activity_watcher(Arc::clone(&session), bus);

    Ok(SpawnedSession { session })
}

fn spawn_reader_thread(
    session: Arc<Session>,
    mut reader: Box<dyn Read + Send>,
    tx: broadcast::Sender<OutputChunk>,
    bus: EventBus,
    history: Option<Arc<SessionHistory>>,
    mut history_log: Option<File>,
    runtime: tokio::runtime::Handle,
) -> Result<()> {
    std::thread::Builder::new()
        .name(format!("pty-read-{}", session.id))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = Bytes::copy_from_slice(&buf[..n]);
                        // Ring and grid advance together under the ring's
                        // lock, so an attach snapshot (which takes the same
                        // lock) never sees a grid ahead of `total_written`.
                        let pos_after = {
                            let mut sb = session.scrollback.lock();
                            sb.push(&data);
                            session.feed_terminal(&data);
                            sb.total_written()
                        };
                        let pos = pos_after - n as u64;
                        let now = Instant::now();
                        *session.last_output.lock() = Some(now);
                        *session.last_output_at.lock() = Some(time::OffsetDateTime::now_utc());
                        session.note_output(now);
                        if !session.first_output_seen.swap(true, Ordering::AcqRel) {
                            crate::launch::record_first_output(
                                &session,
                                session.spawned_at.elapsed(),
                            );
                        }

                        if let Some(log) = history_log.as_mut() {
                            if let Err(e) = log.write_all(&data) {
                                tracing::warn!(
                                    session = %session.id,
                                    error = %e,
                                    "failed to append session history log"
                                );
                                history_log = None;
                            }
                        }

                        let _ = tx.send(OutputChunk { pos, data });

                        let new_state = compute_activity(&session);
                        update_activity_if_changed(&session, new_state, &bus);
                        wake_activity_watcher(&session);
                    }
                    Err(e) => {
                        tracing::debug!(session = %session.id, error = %e, "pty reader closed");
                        break;
                    }
                }
            }
            if let Some(mut log) = history_log {
                let _ = log.flush();
            }
            session.reader_done.store(true, Ordering::Release);
            try_spawn_archive(&session, history, &runtime);
        })
        .map_err(|e| ApiError::Pty(format!("spawn pty reader thread: {e}")))?;
    Ok(())
}

/// Record a session's economics in the Prometheus histograms, once, at the
/// session's real end. A session started without a template is labelled
/// `unknown`. The model is the `--model` its command was launched with, and a
/// session whose command names none carries no model label rather than
/// `unknown`.
/// The `--model` (or `-m`) the session's command was launched with, if it
/// names one. Read from the command the engine spawned, so it is the model
/// the session actually runs rather than a guess from its transcript.
pub(crate) fn launched_model(session: &Session) -> Option<String> {
    let command = session.command()?;
    let args: Vec<&str> = command.split_whitespace().collect();
    let mut last = None;
    let mut args = args.as_slice();
    while let Some((flag, rest)) = args.split_first() {
        if *flag == "--" {
            break;
        }
        if *flag == "--model" || *flag == "-m" {
            last = rest
                .first()
                .filter(|value| !value.starts_with('-'))
                .copied();
        } else if let Some(value) = flag.strip_prefix("--model=") {
            if !value.is_empty() {
                last = Some(value);
            }
        }
        args = rest;
    }
    last.map(str::to_string)
}

pub(crate) fn observe_session_economics(session: &Session) {
    let Some(snapshot) = session.economics_snapshot() else {
        return;
    };
    let Ok(json) = serde_json::to_string(&snapshot) else {
        return;
    };
    observe_economics(session.template(), launched_model(session), Some(&json));
}

/// Record a stored economics snapshot. `json` is the snapshot as the tailer
/// persists it; a record without one records nothing.
pub(crate) fn observe_economics(
    template: Option<String>,
    model: Option<String>,
    json: Option<&str>,
) {
    let Some(json) = json else {
        return;
    };
    let Ok(snapshot) = serde_json::from_str::<vogt_engine_contract::SessionEconomics>(json) else {
        return;
    };
    let mut owned = vec![(
        "template",
        template.unwrap_or_else(|| "unknown".to_string()),
    )];
    if let Some(model) = model {
        owned.push(("model", model));
    }
    let labels: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let metrics = crate::metrics::metrics();
    metrics
        .session_turns
        .observe(&labels, snapshot.turns as f64);
    metrics
        .session_peak_context
        .observe(&labels, snapshot.peak_context_tokens as f64);
    metrics
        .session_compactions
        .observe(&labels, snapshot.compactions as f64);
}

fn spawn_exit_waiter(
    session: Arc<Session>,
    bus: EventBus,
    history: Option<Arc<SessionHistory>>,
    runtime: tokio::runtime::Handle,
    on_exit: Option<ExitHook>,
) {
    tokio::task::spawn_blocking(move || {
        let mut child = match session.child.lock().take() {
            Some(c) => c,
            None => return,
        };
        let status = match child.wait() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(session = %session.id, error = %e, "child wait failed");
                return;
            }
        };
        let code = status.exit_code() as i32;
        *session.ended_at.lock() = Some(time::OffsetDateTime::now_utc());
        *session.exit_code.lock() = Some(code);
        let new_state = compute_activity(&session);
        update_activity_if_changed(&session, new_state, &bus);
        wake_activity_watcher(&session);

        // The child can exit before the PTY reader has consumed the final
        // bytes.  Publish the terminal event only after that reader has
        // drained its stream; agent-task conclusions inspect scrollback when
        // they receive `SessionKilled` (including VOGT_SKIP and VOGT_COST
        // sentinels printed immediately before exit).
        while !session.reader_done.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        bus.publish(ServerEvent::SessionKilled {
            id: session.id,
            exit_code: Some(code),
        });
        if let Some(hook) = on_exit {
            hook(&session);
        }
        // A hibernation stops the process too, but the session is not over:
        // a wake resumes it. It is counted when its hibernated record is
        // removed, which is the end it does not come back from.
        if !session.is_hibernating() {
            observe_session_economics(&session);
        }
        try_spawn_archive(&session, history, &runtime);
    });
}

fn try_spawn_archive(
    session: &Arc<Session>,
    history: Option<Arc<SessionHistory>>,
    runtime: &tokio::runtime::Handle,
) {
    let Some(history) = history else {
        return;
    };
    if !session.reader_done.load(Ordering::Acquire) {
        return;
    }
    let Some(exit_code) = session.exit_code() else {
        return;
    };
    if session
        .archive_started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let session = Arc::clone(session);
    runtime.spawn(async move {
        let output_bytes = read_history_output(&session).await;
        // strip_ansi is CPU-bound over up to 16 MiB; run it off the async
        // worker so it cannot stall other sessions' tasks.
        let visible_output = tokio::task::spawn_blocking(move || strip_ansi(&output_bytes))
            .await
            .unwrap_or_default();
        let output_text = String::from_utf8_lossy(&visible_output);
        let scrollback_bytes = session.scrollback.lock().total_written();
        let record = ArchiveRecord {
            id: session.id,
            name: session.name(),
            created_at: session.created_at,
            ended_at: (*session.ended_at.lock()).or(Some(time::OffsetDateTime::now_utc())),
            exit_code: Some(exit_code),
            cwd: Some(session.cwd.clone()),
            command: session.command.clone(),
            scrollback_bytes,
            end_reason: Some(if session.is_hibernating() {
                crate::history::END_HIBERNATED
            } else if session.stop_requested() {
                crate::history::END_STOPPED
            } else {
                crate::history::END_EXITED
            }),
            identity: None,
            economics: session
                .economics_snapshot()
                .and_then(|snapshot| serde_json::to_string(&snapshot).ok()),
        };

        if let Err(e) = history
            .archive_session_with_output(record, output_text.as_ref())
            .await
        {
            tracing::warn!(session = %session.id, error = %e, "failed to archive session history");
        }
    });
}

/// Archive a still-live session to history immediately.
///
/// Used by the graceful-shutdown drain: a long-lived agent shell that never
/// runs `exit` would otherwise be SIGKILLed on redeploy with nothing archived,
/// because the ordinary archive path (`try_spawn_archive`) waits for the exit
/// waiter to set `exit_code`. This records the row directly. `ended_at` is the
/// child's real end time if it already exited, otherwise now; `exit_code` is
/// whatever the child reported (`None` == terminated/unknown), which the
/// upsert's `COALESCE` guard will not clobber for an already-finalized row.
pub async fn archive_live_session(session: &Arc<Session>, history: &SessionHistory) {
    let output_bytes = read_history_output(session).await;
    let visible_output = strip_ansi(&output_bytes);
    let output_text = String::from_utf8_lossy(&visible_output);
    let scrollback_bytes = session.scrollback.lock().total_written();
    let record = ArchiveRecord {
        id: session.id,
        name: session.name(),
        created_at: session.created_at,
        ended_at: (*session.ended_at.lock()).or(Some(time::OffsetDateTime::now_utc())),
        exit_code: session.exit_code(),
        cwd: Some(session.cwd.clone()),
        command: session.command.clone(),
        scrollback_bytes,
        end_reason: Some(if session.exit_code().is_some() {
            crate::history::END_EXITED
        } else {
            crate::history::END_ENGINE_SHUTDOWN
        }),
        identity: None,
        economics: session
            .economics_snapshot()
            .and_then(|snapshot| serde_json::to_string(&snapshot).ok()),
    };
    if let Err(e) = history
        .archive_session_with_output(record, output_text.as_ref())
        .await
    {
        tracing::warn!(session = %session.id, error = %e, "failed to archive live session during drain");
    }
}

/// Cap on how much of a session's raw log is read into memory to build its
/// searchable archive. A long-running agent session can write
/// gigabytes; reading the whole file (then stripping it, a second copy) risked
/// multi-GB transient RAM and a single gigabyte-scale FTS INSERT. The last
/// 16 MiB of stripped output is what search snippets realistically need.
const HISTORY_INDEX_MAX_BYTES: u64 = 16 * 1024 * 1024;

async fn read_history_output(session: &Arc<Session>) -> Vec<u8> {
    if let Some(path) = session.history_log_path.as_ref() {
        match read_tail(path, HISTORY_INDEX_MAX_BYTES).await {
            Ok(bytes) => return bytes,
            Err(e) => {
                tracing::warn!(
                    session = %session.id,
                    path = %path.display(),
                    error = %e,
                    "failed to read session history log; falling back to scrollback"
                );
            }
        }
    }
    session.snapshot().0.to_vec()
}

/// Read at most the last `max` bytes of a file, without pulling the whole thing
/// into memory when it is larger.
async fn read_tail(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    if len > max {
        file.seek(std::io::SeekFrom::Start(len - max)).await?;
    }
    let mut buf = Vec::with_capacity(len.min(max) as usize);
    file.read_to_end(&mut buf).await?;
    Ok(buf)
}

fn spawn_activity_watcher(session: Arc<Session>, bus: EventBus) {
    tokio::spawn(async move {
        loop {
            if session.exit_code.lock().is_some() {
                break;
            }

            if session.activity() != ActivityState::Running {
                // Already at rest, but the prompt can land after the output
                // that ended the turn: a window title set a moment later, or a
                // box drawn once the burst settles (WI-1519). One more look
                // when the screen changes, so that rest still becomes `ready`.
                if session.activity() == ActivityState::Idle {
                    announce_ready_if_at_prompt(&session, ActivityState::Idle, &bus);
                }
                session.activity_notify.notified().await;
                continue;
            }

            // Measured from the last change to the screen, not the last
            // byte: an identical repaint is not work (WI-1090).
            let reference = session.last_change().unwrap_or(session.spawned_at);
            let elapsed_ms = reference.elapsed().as_millis() as u64;
            let wait_ms = if elapsed_ms >= session.idle_after_ms {
                let new_state = if session.last_output.lock().is_some() {
                    // Settle pending output before classifying.
                    let _ = session.last_change();
                    compute_activity(&session)
                } else {
                    ActivityState::Idle
                };
                update_activity_if_changed(&session, new_state, &bus);
                if session.activity() != ActivityState::Running {
                    continue;
                }
                // Still running though the screen is quiet: a `working`
                // title holds the turn open (WI-1598). Wait a full window
                // before asking again — looping straight back never yields,
                // pinning a runtime worker and starving every other task on
                // it, terminal input included (WI-1644).
                session.idle_after_ms
            } else {
                session.idle_after_ms - elapsed_ms
            };

            let epoch = session.activity_epoch.load(Ordering::Acquire);
            let sleep = tokio::time::sleep(std::time::Duration::from_millis(wait_ms));
            tokio::pin!(sleep);

            tokio::select! {
                _ = session.activity_notify.notified() => {}
                _ = &mut sleep => {
                    if session.activity_epoch.load(Ordering::Acquire) != epoch {
                        continue;
                    }
                    if session.exit_code.lock().is_some() {
                        break;
                    }
                    let new_state = if session.last_output.lock().is_some() {
                        // Settle pending output before classifying.
                        let _ = session.last_change();
                        compute_activity(&session)
                    } else {
                        ActivityState::Idle
                    };
                    update_activity_if_changed(&session, new_state, &bus);
                }
            }
        }
    });
}

fn wake_activity_watcher(session: &Session) {
    session.activity_epoch.fetch_add(1, Ordering::AcqRel);
    session.activity_notify.notify_one();
}

/// How much of the raw tail is scanned for a possible permission dialog. The
/// dialog's question and its countdown sit at the bottom of the screen, so a
/// redraw of them is always recent output.
const APPROVAL_SCAN_BYTES: usize = 4096;

/// Whether the window title announces exactly this state, for either agent
/// whose title this engine reads.
fn title_says(title: Option<&str>, state: crate::screen::AgentTitle) -> bool {
    crate::screen::agent_title(title).is_some_and(|(_, found)| found == state)
}

/// How an agent's window title revises the state the quiet window computed.
///
/// `ready` only ever moves toward rest, and only when the screen does not show
/// a turn: anything in the session can set a title. `working` moves the other
/// way, because the fork's `agent-state` title changes before the screen does
/// (WI-1598). An untitled agent, or one whose title this engine does not read,
/// is left as the quiet window classified it.
fn titled_activity(
    state: ActivityState,
    titled: bool,
    title: Option<&str>,
    screen_busy: bool,
) -> ActivityState {
    if !titled {
        return state;
    }
    if title_says(title, crate::screen::AgentTitle::Ready)
        && state == ActivityState::Running
        && !screen_busy
    {
        ActivityState::Idle
    } else if title_says(title, crate::screen::AgentTitle::Working)
        && matches!(state, ActivityState::Idle | ActivityState::WaitingForInput)
    {
        ActivityState::Running
    } else {
        state
    }
}

/// A permission dialog inferred from the window title alone, when the rendered
/// screen showed none.
///
/// The WI-1127 fork's `agent-state` title and Klaudia's `awaiting approval`
/// both name a dialog the screen parser does not always see (WI-1145,
/// WI-1279). It is a `permission`, which is what the person gate refuses an
/// agent for, and it offers no options: answering it fails before any key is
/// written into the PTY, so a title cannot be used to press a choice.
fn title_approval_dialog(title: Option<&str>) -> Option<crate::approval::Detected> {
    title_says(title, crate::screen::AgentTitle::AwaitingApproval).then(|| {
        crate::approval::Detected {
            question: "awaiting approval".to_string(),
            command_excerpt: String::new(),
            deadline_seconds: None,
            kind: "permission",
            options: Vec::new(),
            horizontal: false,
        }
    })
}

fn compute_activity(session: &Arc<Session>) -> ActivityState {
    let tail = {
        let sb = session.scrollback.lock();
        sb.tail(APPROVAL_SCAN_BYTES).to_vec()
    };
    // Not settled here: this runs for every chunk the reader takes, and
    // `note_output` has already settled any chunk that arrived when the last
    // change was old enough to matter. Output still pending is within one
    // interval of a change, which reads `running` either way. The watcher
    // settles before it asks.
    let last = session.screen_change.lock().changed();
    let classify_from = tail.len().saturating_sub(2048);
    let state = classify(
        last,
        &tail[classify_from..],
        session.idle_after_ms,
        session.exit_code(),
    );
    if state.is_terminal() {
        *session.approval.lock() = None;
        return session.terminal_state().unwrap_or(state);
    }
    // The title outranks the quiet window in both directions (WI-1598). A
    // `ready` title collapses a still-drawing TUI to rest, and a `working`
    // title keeps the turn running while the TUI has gone quiet: the fork's
    // agent-state title turns to `codex: working` at the start of a turn,
    // before the screen has changed enough to move the change clock, so the
    // quiet window alone reads `idle` while the title says working. Only for
    // an agent whose title this engine reads, and never over a terminal state.
    let titled = matches!(session.agent().as_deref(), Some("klaudia" | "codex"));
    let screen_busy = crate::screen::klaudia_busy(&session.render_screen(0).0.lines);
    let state = titled_activity(state, titled, session.title().as_deref(), screen_busy);
    // A permission dialog overrides whatever the tail heuristics said: its
    // countdown keeps redrawing (so it reads `running`) and its menu ends in
    // `❯ 1.` (so it reads `waiting-for-input`), and either way a driver must
    // answer it rather than type at it. Confirmed on the rendered screen —
    // only rendered when the raw tail mentions a dialog — so one that was
    // answered and redrawn over is not reported from text lingering in the
    // tail. When the screen shows nothing but the title says the agent is
    // awaiting approval, the title stands in: the fork's `agent-state` title
    // and Klaudia's `awaiting approval` both name dialogs the screen parser
    // does not always recognise.
    let detected = if crate::approval::mentions_approval(&strip_ansi(&tail)) {
        let (rendered, scrollback) =
            session.render_screen(crate::approval::SCROLLBACK_CONTEXT_LINES);
        crate::approval::detect(&rendered.lines, &scrollback)
    } else {
        None
    };
    let detected = detected.or_else(|| title_approval_dialog(session.title().as_deref()));
    let mut pending = session.approval.lock();
    match detected {
        Some(found) => {
            let now = time::OffsetDateTime::now_utc();
            match pending.as_mut() {
                // The same dialog seen again (its countdown ticking): keep the
                // first sighting, so the deadline does not slide — but take the
                // menu as it is now. A dialog first seen mid-draw has only part
                // of its menu, and the highlight moves under arrow keys.
                Some(p) if p.is_same(&found) => {
                    p.detected.options = found.options;
                    p.detected.kind = found.kind;
                }
                _ => *pending = Some(crate::approval::Pending::new(found, now)),
            }
            ActivityState::AwaitingApproval
        }
        None => {
            *pending = None;
            state
        }
    }
}

/// Store and announce a new activity state.
///
/// Once the child has exited the state is pinned to `exited`/`errored`: the
/// PTY reader keeps draining the last bytes after the exit waiter has run,
/// and a live state it computed from those bytes (before it saw the exit
/// code) must not overwrite the terminal one — that is how a stopped session
/// used to read `running` forever. The exit code is re-read under the
/// activity lock, and the event is published under it too, so subscribers
/// see the transitions in the order they were stored.
fn update_activity_if_changed(session: &Arc<Session>, new: ActivityState, bus: &EventBus) {
    let (settled, ended) = {
        let mut a = session.activity.lock();
        let new = session.terminal_state().unwrap_or(new);
        if *a == new {
            return;
        }
        let now = time::OffsetDateTime::now_utc();
        // A turn starts when the session goes to work from rest; a permission
        // dialog in the middle of a turn, answered, resumes the same turn.
        if begins_turn(*a, new) {
            *session.turn_started_at.lock() = Some(now);
            // The next rest at a prompt is a new turn's readiness (WI-1519).
            session.ready_announced.store(false, Ordering::Release);
            // Likewise a done declaration belongs to the turn it ended (WI-1518).
            *session.done_at.lock() = None;
            session.turn_since_report.store(true, Ordering::Release);
            // Input that arrived at rest (or during the last turn) is what
            // this turn acts on.
            let _ = session.unblock.compare_exchange(
                UNBLOCK_ARMED,
                UNBLOCK_ADDRESSING,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        if new != ActivityState::AwaitingApproval {
            *session.approval.lock() = None;
        }
        // An exited agent is blocked on nobody.
        if new.is_terminal() {
            *session.blocked.lock() = None;
            session.reset_unblock();
        }
        let came_to_rest = (*a == ActivityState::Running || *a == ActivityState::Idle)
            && new == ActivityState::Idle;
        let ended = ends_turn(*a, new);
        *a = new;
        *session.activity_since.lock() = Instant::now();
        *session.activity_changed_at.lock() = now;
        let activity_changed_at = format_rfc3339(now);
        bus.publish(ServerEvent::Activity {
            id: session.id,
            state: new,
            activity_changed_at,
            approval: session.approval_prompt(),
            turn_started_at: session.turn_started_at().map(format_rfc3339),
        });
        (came_to_rest.then_some(new), ended)
    };
    // The clear runs after the activity lock is released (WI-1314).
    if ended {
        clear_addressed_block(session, bus);
    }
    // A worker that finished a turn and rests at its prompt is ready, even
    // when the activity reads `idle` rather than `waiting-for-input`: Claude
    // Code's boxed prompt rarely matches the waiting patterns, and Klaudia and
    // Codex are forced to `idle` by their window title (WI-1519). `idle` itself
    // stays out of the change feed — a session that went quiet mid-turn is not
    // ready — so readiness is its own event, and at most one of them per turn.
    // The render happens after the activity lock is dropped: reading the grid
    // under it would stall the PTY reader on a screen render.
    if let Some(resting) = settled {
        announce_ready_if_at_prompt(session, resting, bus);
    }
}

/// Publish one `ready` change when `session` has just gone idle and its screen
/// shows a prompt a driver can type into. At most one per turn: an earlier
/// rest in the same turn that was not at a prompt does not count, because the
/// prompt often lands a moment after the output that began the turn.
fn announce_ready_if_at_prompt(session: &Session, resting: ActivityState, bus: &EventBus) {
    if session.ready_announced.load(Ordering::Acquire) {
        return;
    }
    let (rendered, _) = session.render_screen(0);
    let ready = crate::screen::is_ready(
        resting,
        session.is_alive(),
        &rendered.lines,
        session.agent().as_deref(),
        rendered.title.as_deref(),
    );
    if ready && !session.ready_announced.swap(true, Ordering::AcqRel) {
        bus.publish(ServerEvent::SessionReady { id: session.id });
    }
}

/// The blocked report has not been addressed.
const UNBLOCK_NONE: u8 = 0;
/// Input arrived; the next turn to start acts on it.
const UNBLOCK_ARMED: u8 = 1;
/// A turn acting on the input (or a dialog answer) is under way; its end
/// clears the report.
const UNBLOCK_ADDRESSING: u8 = 2;

/// A turn ends when the session comes back to rest. A permission dialog is
/// the middle of a turn, so reaching one does not end it.
fn ends_turn(old: ActivityState, new: ActivityState) -> bool {
    matches!(new, ActivityState::Idle | ActivityState::WaitingForInput)
        && matches!(
            old,
            ActivityState::Running | ActivityState::AwaitingApproval
        )
}

/// Why a turn's end clears the standing blocked report, if it does.
fn unblock_cause(
    addressed: bool,
    turn_since_report: bool,
    says_done: bool,
) -> Option<&'static str> {
    if addressed {
        Some("addressed")
    } else if turn_since_report && says_done {
        Some("autopilot-done")
    } else {
        None
    }
}

/// At a turn's end, drop a blocked report that turn dealt with (WI-1314):
/// the turn acted on input or a dialog answer given after the report, or a
/// turn begun after the report ended with `AUTOPILOT: DONE` on screen. The
/// same `SessionBlocked { blocked: None }` that `report_unblocked` publishes
/// goes out, so `session.changes`, sweeps and `wait_any` see it. Runs with
/// the activity lock released: the screen render takes its own locks.
fn clear_addressed_block(session: &Arc<Session>, bus: &EventBus) {
    if session.blocked.lock().is_none() {
        return;
    }
    let addressed = session.unblock.load(Ordering::Acquire) == UNBLOCK_ADDRESSING;
    let turn_since_report = session.turn_since_report.load(Ordering::Acquire);
    let says_done = !addressed
        && turn_since_report
        && crate::autopilot::says_done(&session.render_screen(0).0.lines);
    let Some(cause) = unblock_cause(addressed, turn_since_report, says_done) else {
        return;
    };
    if session.blocked.lock().take().is_none() {
        return;
    }
    session.reset_unblock();
    tracing::info!(
        session = %session.id,
        event = "session.unblocked",
        cause,
        "cleared a blocked report the session has dealt with"
    );
    bus.publish(ServerEvent::SessionBlocked {
        id: session.id,
        blocked: None,
    });
}

/// Whether moving from `old` to `new` starts a new turn.
fn begins_turn(old: ActivityState, new: ActivityState) -> bool {
    new == ActivityState::Running
        && matches!(old, ActivityState::Idle | ActivityState::WaitingForInput)
}

#[cfg(test)]
mod economics_tests {
    use super::observe_economics;

    fn count_for(label: &str) -> u64 {
        crate::metrics::metrics()
            .render()
            .lines()
            .find(|line| line.starts_with(&format!("vogt_session_turns_count{{{label}}}")))
            .and_then(|line| line.rsplit(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }

    /// Observing is what the exit waiter and the removal of a hibernated
    /// record both call, and each calls it exactly once. A missing model is
    /// no label, not `unknown`.
    #[test]
    fn observing_counts_once_and_omits_an_unnamed_model() {
        let json = r#"{"turns":4,"context_tokens":10,"peak_context_tokens":10,"compactions":1,"tool_result_bytes":{},"reads_repeated":0,"wall_clock":{"count":0,"max_ms":0,"mean_ms":0},"output_tokens":0,"usage_measured":false}"#;
        let label = "template=\"wi-1245-observe\"";
        let before = count_for(label);
        observe_economics(Some("wi-1245-observe".into()), None, Some(json));
        assert_eq!(count_for(label), before + 1);
        observe_economics(Some("wi-1245-observe".into()), None, None);
        observe_economics(Some("wi-1245-observe".into()), None, Some("not json"));
        assert_eq!(
            count_for(label),
            before + 1,
            "a record without a snapshot adds nothing"
        );
        // The message names the label, not the rendered text: that text holds
        // every family, including the secret-read counter, and has no place in
        // a test log.
        let rendered = crate::metrics::metrics().render();
        assert!(
            !rendered.contains("template=\"wi-1245-observe\",model="),
            "an unnamed model was labelled on the wi-1245-observe series"
        );
    }
}

#[cfg(test)]
mod turn_tests {
    use super::{begins_turn, ends_turn, unblock_cause, ActivityState::*};

    #[test]
    fn a_turn_starts_from_rest_and_survives_a_permission_dialog() {
        assert!(begins_turn(Idle, Running));
        assert!(begins_turn(WaitingForInput, Running));
        // Answering a permission dialog continues the same turn.
        assert!(!begins_turn(AwaitingApproval, Running));
        assert!(!begins_turn(Running, Running));
        assert!(!begins_turn(Running, Idle));
        assert!(!begins_turn(Idle, AwaitingApproval));
    }

    /// The sequence the fork actually writes (WI-1598): `codex: ready` at rest,
    /// `codex: working` at the start of a turn — before the screen has changed,
    /// so the quiet window still says idle — then `codex: awaiting approval`
    /// for an MCP elicitation, then `codex: ready` again. The title has to
    /// carry the activity through that, not only demote a busy screen to idle.
    #[test]
    fn a_working_title_is_running_while_the_quiet_window_says_idle() {
        use crate::activity::classify;
        use std::time::{Duration, Instant};

        let quiet = Instant::now() - Duration::from_secs(10);
        let titles = [
            ("codex: ready", false),
            ("codex: working", true),
            ("codex: working · editing", true),
            ("codex: awaiting approval", false),
            ("codex: ready", false),
        ];
        let mut seen_running = false;
        for (title, working) in titles {
            // The screen has not changed, so the quiet window reads idle.
            let from_quiet = classify(Some(quiet), b"", 1_500, None);
            assert_eq!(from_quiet, Idle, "{title}");
            let state = super::titled_activity(from_quiet, true, Some(title), false);
            if working {
                assert_eq!(state, Running, "{title} must read running");
                seen_running = true;
            } else if title.contains("approval") {
                // The title dialog, applied after this, is what makes it
                // awaiting-approval. This step must not have called it idle.
                assert_ne!(state, Running, "{title}");
            } else {
                assert_eq!(state, Idle, "{title} is at rest");
            }
        }
        assert!(seen_running);

        // A title anything in the session could have set does not outrank a
        // screen that shows a turn, and an agent with no title of its own
        // stays on the quiet window.
        let recent = classify(Some(Instant::now()), b"drawing", 1_500, None);
        assert_eq!(
            super::titled_activity(recent, true, Some("codex: ready"), true),
            Running
        );
        assert_eq!(
            super::titled_activity(Idle, false, Some("codex: working"), false),
            Idle
        );
    }

    #[test]
    fn a_turn_ends_on_returning_to_rest_not_at_a_dialog() {
        // WI-1314: an addressed block clears when the turn acting on it ends.
        assert!(ends_turn(Running, Idle));
        assert!(ends_turn(Running, WaitingForInput));
        assert!(ends_turn(AwaitingApproval, Idle));
        assert!(!ends_turn(AwaitingApproval, Running));
        assert!(!ends_turn(Running, AwaitingApproval));
        assert!(!ends_turn(Idle, Running));
        assert!(!ends_turn(Idle, WaitingForInput));
    }

    #[test]
    fn a_turn_end_clears_only_an_addressed_or_done_block() {
        assert_eq!(unblock_cause(true, true, false), Some("addressed"));
        assert_eq!(unblock_cause(true, false, false), Some("addressed"));
        assert_eq!(unblock_cause(false, true, true), Some("autopilot-done"));
        // The reporting reply's own sign-off is not new.
        assert_eq!(unblock_cause(false, false, true), None);
        // Unrelated output: a turn ran, nobody addressed it, no marker.
        assert_eq!(unblock_cause(false, true, false), None);
    }
}

#[cfg(test)]
mod secret_env_tests {
    use super::{is_agent_auth_helper_env, is_secret_env};

    #[test]
    fn agent_auth_helper_regains_only_the_brokering_inputs() {
        // Every variable the helper is re-granted must be one is_secret_env
        // would otherwise strip — the re-grant is meaningless (or a bug) for a
        // variable that already survives sanitisation.
        for key in [
            "INFISICAL_API_URL",
            "INFISICAL_CLIENT_ID",
            "INFISICAL_CLIENT_SECRET",
            "ENGINE_INFISICAL_ENV",
            "ENGINE_AGENT_AUTH_SECRETS",
            "ENGINE_AGENT_AUTH_GH_TOKEN_FROM",
            "ENGINE_AGENT_AUTH_TOKEN_PROJECT_ID",
            "ENGINE_AGENT_AUTH_VOGT_SECRET_NAME",
        ] {
            assert!(
                is_agent_auth_helper_env(key),
                "{key} should reach the helper"
            );
            assert!(
                is_secret_env(key),
                "{key} is re-granted, so it must be one the sanitiser strips"
            );
        }
    }

    #[test]
    fn agent_auth_helper_never_regains_the_broad_engine_secrets() {
        // The helper needs the Infisical identity, not the engine's own token
        // set, assistant/STT keys or the tailscale key.
        for key in [
            "ENGINE_TOKEN",
            "ENGINE_EXTRA_TOKENS_JSON",
            "ENGINE_ASSISTANT_API_KEY",
            "TAILSCALE_AUTH_KEY",
            "VOGT_CORE_TOKEN",
            "GITHUB_TOKEN",
        ] {
            assert!(
                !is_agent_auth_helper_env(key),
                "{key} must not be re-granted to the helper"
            );
        }
    }

    #[test]
    fn engine_credentials_are_denied() {
        for key in [
            "ENGINE_TOKEN",
            "ENGINE_EXTRA_TOKENS_JSON",
            "ENGINE_ASSISTANT_API_KEY",
            "INFISICAL_CLIENT_ID",
            "INFISICAL_CLIENT_SECRET",
            "TAILSCALE_AUTH_KEY",
            "VOGT_CORE_TOKEN",
            "GITHUB_TOKEN",
            "vogt_core_token",
        ] {
            assert!(is_secret_env(key), "{key} should be treated as a secret");
        }
    }

    #[test]
    fn identity_passthrough_reverses_only_the_four_identity_vars() {
        use super::{identity_passthrough_from, is_identity_env};
        // Exactly the four identity strips are what passthrough re-grants,
        // and each must still be one is_secret_env would strip by default.
        for key in [
            "INFISICAL_API_URL",
            "INFISICAL_CLIENT_ID",
            "INFISICAL_CLIENT_SECRET",
            "ENGINE_INFISICAL_ENV",
        ] {
            assert!(is_identity_env(key), "{key} is an identity var");
            assert!(is_secret_env(key), "{key} must be one the sanitiser strips");
        }
        // Passthrough is identity, never the engine's own credentials or the
        // manifest — those stay stripped in both modes (acceptance: ENGINE_TOKEN
        // never present).
        for key in [
            "ENGINE_TOKEN",
            "ENGINE_EXTRA_TOKENS_JSON",
            "ENGINE_ASSISTANT_API_KEY",
            "TAILSCALE_AUTH_KEY",
            "VOGT_CORE_TOKEN",
            "GITHUB_TOKEN",
            "ENGINE_AGENT_AUTH_SECRETS",
        ] {
            assert!(
                !is_identity_env(key),
                "{key} must not be granted by passthrough"
            );
        }
        // The switch is off unless explicitly truthy; default stays brokered.
        for v in ["1", "true", "TRUE", "yes", "on", " 1 "] {
            assert!(
                identity_passthrough_from(Some(v)),
                "{v:?} should enable passthrough"
            );
        }
        for v in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("no"),
            Some("off"),
        ] {
            assert!(
                !identity_passthrough_from(v),
                "{v:?} should not enable passthrough"
            );
        }
    }

    #[test]
    fn ordinary_variables_survive() {
        for key in [
            "PATH",
            "HOME",
            "TERM",
            "LANG",
            "LC_ALL",
            "SHELL",
            "USER",
            "VOGT_ENGINE_SESSION",
            "VOGT_ENGINE_SESSION_ID",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
        ] {
            assert!(!is_secret_env(key), "{key} should not be filtered");
        }
    }

    #[test]
    fn command_launched_helper_is_recognised_by_path_or_basename() {
        use super::is_agent_auth_helper_command;
        use std::path::Path;
        let helper = Path::new("/usr/local/bin/vogt-agent-auth");
        // The engine-chosen path names the helper in full; the "(protected)"
        // templates (`agent-auth run -- <cli>`) name it by its basename on PATH.
        // Both are the helper and must broker the same token set (#727).
        assert!(is_agent_auth_helper_command(
            "/usr/local/bin/vogt-agent-auth",
            helper
        ));
        assert!(is_agent_auth_helper_command("vogt-agent-auth", helper));
    }

    #[test]
    fn a_plain_command_is_not_the_helper() {
        use super::is_agent_auth_helper_command;
        use std::path::Path;
        let helper = Path::new("/usr/local/bin/vogt-agent-auth");
        // A bare shell or any CLI launched directly — not via the helper — must
        // NOT be re-granted the broker manifest.
        assert!(!is_agent_auth_helper_command("bash", helper));
        assert!(!is_agent_auth_helper_command("/bin/bash", helper));
        // A same-named binary at a different absolute path is not the configured
        // helper either.
        assert!(!is_agent_auth_helper_command(
            "/opt/other/vogt-agent-auth",
            helper
        ));
    }
}
