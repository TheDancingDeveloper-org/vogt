# WI-1150 Codex parity bar (vogt-dev)

Runnable acceptance for the ten items on WI-1150, derived from WI-1120 and
the 2026-10-13 vogt-prod run (comment on WI-1150). Do **not** run this against
vogt-dev until oversight says the new Codex fork is live there.

Target engine: `http://100.101.101.57:8910` (override with `VOGT_DEV_ENGINE`).
Auth: a token that may `session.start` / `session.hibernate` / `work.comment`
on that engine (`VOGT_DEV_TOKEN`). Never print the token.

Scratch repos for item 5 live under `/tmp/codex-parity-{a,b}` on the engine
host and are deleted at the end. Do not point them at a shared checkout.

Record the result as one comment on **WI-1122**: a pass/fail table and a
go/no-go for WI-1151 (retire Klaudia). Go requires items 1, 3, 5 and 6 to
pass. A partial is a fail.

`scripts/parity/codex_bar.py` collects the engine-visible evidence (sessions,
titles, hibernate, MCP `board_list`, `agent_cli_list`). Items that need a
person inside the pod (binary, bwrap, rebase, quick-chat flip) are manual
and stay `BLOCKED` until that evidence is pasted in.

## 1. Driven TUI session

Steps:

1. `session_start` `template=codex`, `model=grok-4.7`, `project=vogt`,
   `permission_mode=default`, a short task whose brief exists on disk.
2. Repeat for `accept_edits` and, only with a person's grant, `bypass`.
3. Poll `session_screen` until `ready`. Read the terminal title.
4. Start one more session with `autopilot=true` and a two-step task. Leave it
   idle with work left. Confirm the engine redrives (nudge count ≥ 1, the
   second step starts).
5. `session_hibernate` the default session, then `session_wake` by the same
   `ses_…` id (or `session_input`, which wakes).

Pass:

- Title shows `codex: working` while a turn runs and `codex: ready` when idle,
  and `session_digest` activity matches the title (not stuck `idle` while the
  title says working).
- Command line is `-a on-request -s danger-full-access` for default;
  `accept_edits` adds `approvals_reviewer="auto_review"`; nothing adds
  `--dangerously-bypass-approvals-and-sandbox` unless `bypass` was granted.
- Launch includes `--session-id` equal to the engine session UUID.
- Hibernate returns resumable, and wake resumes that conversation (rollout id
  equals the engine id, or the engine reports the conversation it stored).

Fail: missing `--session-id`, hibernate 409 "does not know an agent
conversation", or activity idle while the title says working (WI-1354,
WI-1355).

## 2. Providers and quick chat

Steps:

1. In the default session, ask which model providers are configured
   (`~/.codex/config.toml` `model_providers`, or `codex debug` / the config
   the process loaded). Capture the name list.
2. One completion each through grok-4.7, DeepSeek, OpenRouter and Bedrock,
   each a single trivial tool-free prompt. Record the provider name and that
   a reply came back.
3. Quick chat: only after an operator sets `ENGINE_CHAT_DRIVER=codex`. Then
   `chat_create` with a one-line message and `wait_s` long enough for a reply.

Pass: all four providers answer, and one quick-chat reply comes from
app-server (`userAgent` contains `codex` and the fork SHA). A catalog of
`['grok']` only is a fail (WI-1360). Quick chat left unflipped is `BLOCKED`,
not a pass.

## 3. MCP: vogt, cadastre, large schemas

Steps:

1. From the Codex session, call vogt `board_list` and cadastre `lookup` (or
   `brief`) once each.
2. Watch `session_screen` `approval` and `session_digest` activity across
   the call.

Pass:

- Both calls return a payload. No HTTP 400 from schema size.
- If an approval prompt appears, `approval` is non-null, activity is
  `awaiting-approval`, and an agent `session_input` / `session_answer` is
  refused with `person_required`. Estate MCP tools may instead be
  pre-approved by the driven-session policy; that is a pass only when no
  prompt is shown and the call still succeeds.

Fail: approval prompt with `approval=null` and activity `idle`, or an agent
typing an approval (WI-1355).

## 4. Sub-agents

Steps, inside the Codex session:

1. `spawn_agent` with an `output_schema` of `{"type":"object","properties":{"count":{"type":"integer"}},"required":["count"]}` and a task that returns `{"count": 2}`.
2. Spawn again with `working_dir` set to a subdirectory of the session cwd.
3. Spawn with worktree isolation and confirm the child cwd is a worktree of
   the parent repo, not the parent cwd.

Pass: schema-valid `{"count": 2}`; the child reports the requested
`working_dir`; the isolated child has its own worktree. Missing `working_dir`
or no isolated tool is a fail (WI-1358, WI-1134, WI-1138).

## 5. gitguard

Steps, only in `/tmp/codex-parity-a` (dirty: one staged and one unstaged
file) and `/tmp/codex-parity-b` (a second repo):

1. Read `/etc/codex/requirements.toml` (or the managed hook the engine
   installed). It must name the gitguard hook, not be comments only.
2. Ask the session to run, and record the exit: `git stash`, `git checkout -- .`,
   `git clean -f`, `git reset --hard`, and `git -C /tmp/codex-parity-b restore .`.
3. A normal `git commit` of a new file in the scratch repo must still work.

Pass: every destructive command is refused by the hook (non-zero, working
tree unchanged). The ordinary commit succeeds. Fail if any destructive
command exits 0 (WI-1356, p0).

## 6. Instructions, skills, hooks, memory

Steps: ask the session to list what it loaded at start.

Pass, all of:

- workspace `AGENTS.md` and `CLAUDE.md`
- `.claude/rules` (the workspace rules)
- `.claude/skills` as well as `.codex/skills`
- the gitguard hook from item 5
- memory readable (a note written in this session is recalled)

Fail if only the repo `AGENTS.md` and `.codex/skills` load (WI-1357).

## 7. Sandbox and approvals

Steps:

1. `codex sandbox linux` (or the posture's sandbox probe) inside the pod.
2. Record the sandbox flag on each posture's command line (item 1).

Pass: bwrap starts a namespace (or the operator has recorded a decision that
the pod will not enable user namespaces, and the posture is then an honest
non-sandbox rather than a silent `danger-full-access`). Approval routing is
the item 3 criterion. `bwrap: No permissions to create a new namespace` with
every posture on `danger-full-access` is a fail (WI-1359).

## 8. App-server and ACP

Steps:

1. `codex app-server` `initialize`. Record `userAgent`.
2. `codex-acp` (or `npx @agentclientprotocol/codex-acp`) against the fork
   binary; one `initialize` handshake.

Pass: userAgent contains the fork version and SHA; ACP initialize completes
within 60s. No `codex-acp` on the pod is a fail (WI-1360).

## 9. Fork installed by SHA

Steps:

1. Read `/usr/local/share/vogt/agent-versions.env` and
   `agent-versions.resolved`.
2. `codex --version`.
3. `agent_cli_list` over MCP.

Pass: `VOGT_CODEX_SHA` matches `codex --version` and the resolved file, and
`agent_cli_list` returns the row (kind `github-release` must not crash the
handler — WI-1362).

## 10. Rebase drill

Steps, on a checkout of the fork, not on this vogt tree:

1. Resolve the next upstream stable tag after the fork's base (the previous
   run used `rust-v0.162.1`).
2. `git rebase --onto <tag> <fork-base> <patch-branch>`.
3. Re-run the fork suite (`cargo test` as the fork's README states).

Pass: the rebase completes with no conflicts and the suite is green. Stopping
on the first commit is a fail (WI-1361). Do not push the rebased branch.

## Output

Post one comment on WI-1122, not a second copy on WI-1150:

| # | Item | Result | Evidence |
|---|------|--------|----------|
| 1..10 | | PASS / FAIL / BLOCKED | command, id, one-line observation |

Then: `Go/no-go for WI-1151: GO` or `NO-GO`, and the WI numbers still open.
