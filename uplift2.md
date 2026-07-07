# Uplift 2

Queued work items. Execute only when explicitly requested.

## Completed

1. Removed the weather and daily briefing surface from `MyDevEnv2`.
   - Removed backend routes and handlers.
   - Removed config fields and environment parsing.
   - Removed related tests.
   - Removed docs references.
2. Added a first-class agent task UI.
   - Added a dedicated Tasks tab and route in the web app.
   - Added task create/edit/pause/resume/run/delete actions.
   - Added recent-run inspection with session-open actions.
   - Added task navigation from the drawer and command palette.
3. Tracked clearer task run outcomes and persisted run history details.
   - Added explicit run status, completion time, exit code, and summary fields.
   - Updated the agent-task registry from session exit events.
   - Reconciled pre-existing in-flight runs on startup after a restart.
   - Surfaced run outcomes in the Tasks UI and integration coverage.
4. Expanded session templates into richer workspace presets.
   - Extended the preset model with tags, repo/path matching, and default-name patterns.
   - Added repo-aware preset launch resolution with placeholder expansion.
   - Added preset creation from workspace directories.
   - Reworked the preset editor around structured command args and matching rules.
5. Extended the command palette into a broader navigation and action surface.
   - Added open-tab navigation commands.
   - Added direct preset-launch commands.
   - Added task run commands alongside task navigation.
   - Kept session, file, git, history, settings, and GUI actions available in one surface.
6. Added higher-level file operations.
   - Added a shared backend file-operation endpoint for move, delete, mkdir, and duplicate.
   - Added tree actions for rename/move, duplicate, delete, and upload.
   - Added folder creation and upload actions to the file-tree header.
   - Verified the new operations through the HTTP integration suite.
7. Expanded the git surface into a fuller workflow.
   - Added a shared backend git-operation endpoint for stage, unstage, discard, commit, and branch checkout/create.
   - Tightened git diff path validation to keep file access repo-relative.
   - Reworked the Git tab with branch controls, staged commit flow, selected-file actions, and staged/worktree diff switching.
   - Extended integration coverage across the end-to-end git workflow.
8. Added saved workspace layouts.
   - Added browser-local named workspace layout storage on top of the existing tab persistence model.
   - Added restore logic that reapplies tab sets, active tab, and layout mode while skipping terminal tabs for sessions that no longer exist.
   - Added layout management in Settings plus save/restore commands in the command palette.
   - Reused existing per-terminal split persistence by restoring saved tab ids directly.
9. Added multi-session orchestration for terminal workspaces.
   - Extended grouped terminal tabs with explicit pane roster and active-pane switching controls.
   - Added broadcast input mode across all panes in a grouped terminal workspace.
   - Routed keyboard input, browser paste, mobile composer input, and workspace shortcut input through the same broadcast gate.
   - Persisted the broadcast setting alongside each saved terminal split layout.
10. Expanded history UX into a stronger inspection and replay surface.
   - Added combined metadata filtering, full-text output search, and session sorting controls.
   - Added replay-oriented log preview and full raw-log export actions.
   - Added browser-local history pinning with cross-tab refresh support.
   - Extended integration coverage for archived-log preview and download routes.

## Enhancement Backlog

1. Add richer push-notification controls such as per-session/task rules,
   quiet hours, and digests.
2. Mature the GUI tab with saved launchers, process labeling, and stream
   health visibility.
3. Improve mobile ergonomics for reconnects, tab management, terminal use, and
   touch-first editing.
4. Improve auth and onboarding UX around bearer token setup and device-local
   profiles.
5. Add lightweight admin and operational visibility inside the app for
   sessions, push, GUI, auth-broker, and storage state.
6. Tighten packaging and release polish around the supported server/PWA/mobile
   surfaces and the legacy client boundary.
7. Clean up module and product-surface boundaries across server, web, mobile,
   and deprecated client code.
8. Consolidate operator and user documentation to reduce drift across repo
   docs.
9. Add user-configurable retention and quota controls for history, scrollback,
   prompts, and storage budgets.
10. Improve workspace search with filename/symbol search and tighter editor
   integration.
11. Add stronger workspace-awareness features such as detected projects,
   task-runner shortcuts, and language-specific quick actions.
