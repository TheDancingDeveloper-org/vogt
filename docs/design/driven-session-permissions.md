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

## Operator decisions

1. **Which repositories are autonomous-merge repositories**: the list goes in
   the deployment's policy file. Suggested: the estate repos agents work on,
   such as arr-diskwarden and indexarr/ops.
2. **Who may grant `bypass`**: implemented as `admin` scope. The alternative
   is any `work.write` caller, which would let an agent escalate its children.
3. **Where the estate file lives**: in the ops overlay of the stack (beside
   `estate.overlay.yml`), mounted, and named by `ENGINE_AGENT_CLAUDE_SETTINGS`.
