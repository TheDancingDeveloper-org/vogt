# Permission posture of driven sessions (WI-926)

Status: **implemented** in the PR that adds this note. Where this note and
[`ENGINE.md`](../ENGINE.md) disagree, `ENGINE.md` describes what exists.

## What happens today

Nothing in Vogt sets a permission mode. The engine launches
`vogt-agent-auth run -- claude` with no permission flag, and no settings file
in the pod sets `permissions.defaultMode`. Driven sessions run in Claude
Code's own default, which on this pod is **auto mode**: a classifier judges
each action against natural-language rules, and the defaults include `soft_deny` rules
such as *Merge Without Review*, *Modify Shared Resources*, *Production
Deploy* and *Credential Materialization*. In an interactive session an auto-mode
block is a **denial, not a dialog**. Nobody is asked, so nothing is answerable,
and the agent has to stop. That is how `sonarr-imports` could not merge its
own green PRs (arr-diskwarden #21, ops #21) on 2026-10-05, while the
oversight runner, started with `--dangerously-skip-permissions`, could.

## Model

Three postures, in order of preference:

1. **Auto mode with a scoped policy (the default).** Every engine-launched
   Claude session is given a Vogt-managed settings file (`--settings`) whose
   `autoMode` section adds to Claude Code's built-in rules, through the
   literal `"$defaults"` entry, and replaces none of them. Prod-mutating,
   destructive, shared-resource and secret-exposing actions keep gating,
   because their rules are untouched. The policy adds:
   - an **environment** entry naming the repositories where an agent may
     merge its own work (`Autonomous-merge repositories`), which the
     deployment fills in; *none configured* by default;
   - one **allow** exception, *Own Green PR Merge*. It covers merging a pull
     request the agent opened for its task, in a listed repository, once the
     repository's required checks have passed, with the plain merge command.
     `--admin`, force options, bypassed reviews and disabled checks stay
     blocked.

   The shipped policy changes nothing until a deployment lists repositories.
   A deployment adds its own estate facts (secret stores, deploy targets,
   sensitive hosts) in the same file, so the classifier knows what "prod"
   means there.
2. **Per-spawn opt-in** on `session.start`: `permission_mode`
   - `default` keeps the posture above;
   - `accept_edits` uses Claude Code's `acceptEdits` mode: file edits are
     auto-accepted, and everything else prompts;
   - `bypass` uses `--dangerously-skip-permissions`: no checks at all.

   `bypass` is never the default. It needs the `admin` scope, so an agent
   session cannot hand a child the autonomy it lacks itself (the session token
   holds every scope except `admin`). It is recorded on the audit row and shown
   on the session row.
3. **The oversight runner** stays the single standing bypassed session, started
   by the operator as before.

## Why not the other knobs

- **`permissions.allow` rules** (`Bash(gh pr merge:*)`) are pattern matches.
  They would also allow `gh pr merge --admin` on any repository, and allow
  rules are evaluated before the classifier, so nothing would judge the call.
  The auto-mode exception is judged in context: whose PR, which repository,
  whether the checks passed.
- **Blanket bypass** removes every guardrail. On 2026-10-05 those guardrails
  gated a Komodo stack env change and a raw secret print, both of which should
  have gated.
- **"Defer to the driver" (WI-917)** would be the better long-term shape, but
  auto mode does not produce a prompt to answer: a block is a denial.
  `session_answer` can only answer dialogs the CLI draws (`manual` /
  `acceptEdits` prompts, startup gates). So today's deferral path is the
  blocked report. The autopilot and driving briefs now tell an agent whose
  action was denied **not to retry or work around it**, and to call
  `session_report_blocked` with the action, the denial and what a person needs
  to do. That raises it in the Inbox and the Oversight sweep (WI-915), where a
  driver or the operator acts on it. If Claude Code later offers
  ask-instead-of-deny in auto mode, `session_answer` can answer it directly.

## Where the policy lives

- The image ships `/usr/local/share/vogt/driven-session-settings.json` (source
  `engine/deploy/driven-session-settings.json`).
- `ENGINE_AGENT_CLAUDE_SETTINGS` points the engine at a deployment's own file
  instead. Empty means the image's policy, and `off` turns the policy off.
- Settings are versioned with the deployment (the ops overlay). They are not
  per project, because the estate facts are the same for every session on a
  stack. A per-template override can come later if a stack needs one.

## Operator decisions (2026-10-05)

1. **Autonomous-merge repositories**: TheDancingDeveloper-org/arr-diskwarden,
   TheDancingDeveloper-org/vogt and indexarr/ops. These are listed in the
   estate's policy file, not in the shipped default, which stays *none
   configured*.
2. **Who may grant `bypass`**: a person only. Every agent is refused,
   the oversight runner included (as implemented).
3. **Where the estate policy lives**: the ops overlay. Each vogt stack has
   `personal/<stack>/driven-session-settings.json`, mounted read-only at
   `/run/vogt/driven-session-settings.json` and named by
   `ENGINE_AGENT_CLAUDE_SETTINGS` (indexarr/ops PR #23). It also names the
   estate's secret store (Infisical), deploy path (Komodo) and sensitive
   targets (the Node B prod stacks), so those keep gating.

## opencode (WI-932, 2026-10-05)

The postures above were Claude Code only. A driven opencode session stalled
at "Access external directory" with nobody to answer. To stop that,
javascan's operator edited the **shared** `~/.config/opencode/opencode.jsonc`
to allow every tool, `bash` included, for every opencode session on the pod,
including ones a person starts. That is the blanket bypass this design
rejects, applied globally.

opencode reads an inline config from `OPENCODE_CONFIG_CONTENT` and layers it
over the user's own, for that process only. The engine uses it for each
session's posture:
- **default:** the deployment's opencode policy
  (`ENGINE_AGENT_OPENCODE_CONFIG`; the image ships
  `engine/deploy/driven-session-opencode.json`). Routine tools are allowed.
  Destructive, shared-resource and secret-exposing commands are **denied**
  rather than asked, because a prompt nobody answers is the stall this
  exists to end, and a denial is reported.
- **accept-edits:** edits allowed, the rest asks.
- **bypass:** everything allowed, a person's grant only.

opencode matches command patterns rather than judging context, so its list
is coarser than Claude Code's classifier. The shipped default denies `gh pr
merge` outright, matching "no autonomous-merge repositories configured". A
deployment that lists repositories allows it in its own file. Once this is
deployed, the shared config's blanket `"bash": "allow"` is overridden for
every engine-launched session, and can be removed so that sessions a person
starts by hand ask again.

## Agents in engine-started sessions (WI-926, 2026-10-06)

Dev validation found the bypass rule bypassed. An agent in a session the
engine started itself (from the GUI or a protected template) asked for a
`bypass` child and got one. Its `VOGT_HTTP_TOKEN` was the pod's brokered
token, issued to the person `local:vogt`, so vogt-core counted the agent as
that person.

The engine now asks vogt-core for a credential of the session's own
(`session.token`, an operation only the engine's credential may call) for
every Claude Code, Codex or opencode session it launches without one, and
revokes it when the session ends. The token is bound to the agent actor
`agent:engine:<engine id>` with `agent_session_scopes`, the same scopes
`session.start` mints for its own sessions. Such sessions are now refused
`bypass`, and their writes are attributed to the session rather than to the
person.

A plain shell keeps the pod's token: a person is at it, so their
attribution does not change. An agent a person starts by hand in that shell
inherits the shell's token, and that is their choice to make.

## Read-only relief (2026-10-07)

Driven sessions were denied reads: `docker ps` and `docker inspect` as
*Modify Shared Resources* or *Interfere With Workloads*, and checking whether
a credential variable is set as credential materialization. The shipped
policy adds two exceptions in the classifier's own wording, and no pattern
rule:

- **Read-Only Inspection** clears those two rules for an exhaustive list
  of commands that only report state; a command it does not name is not
  covered. It does not cover secret printing, environment dumps
  (`.Config.Env` or a whole-object inspect), `docker exec`, any other
  `docker` subcommand, Komodo, Infisical, non-loopback or non-GET requests.
- **Credential Presence Check** yields only set or unset for a credential
  the session already holds: never a length, substring, hash or comparison,
  and never another process's environment, `/run/secrets` or the vault.
  Fetching stays with the default rules and the grant flow (WI-973).

Both texts are golden-pinned by the engine's `driven_policy_tests`, so a
rewording is a deliberate, reviewed change. A deployment's own policy file
replaces the image's, so it has to carry the same rules for them to apply.

Pattern allow rules were rejected because they resolve before the
classifier and cannot tell a harmless `docker inspect --format` from an
environment dump, or a health-check GET from a curl that carries a body.

**Not changed: merge as a prompt.** An explicit `permissions.ask` rule is
the only way to make auto mode prompt rather than deny. But any agent with
`work.write` can answer a session's prompt (`session.answer`,
`session.input`), so an ask rule on `gh pr merge` would let one agent
approve another's merge. That waits on WI-983. An ask rule on `git push`
was also rejected: ordinary pushes already pass, a rule would make every
push prompt, and it would turn a classifier-denied force-push into a
prompt.

Whether the classifier honours the new exceptions is judged at run time.
The engine's unit tests pin the file's shape, and the verdicts need a
check on vogt-dev.
