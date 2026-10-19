# Python test disposition (WI-1080)

Every `tests/test_*.py` on this branch has exactly one row. The Rust core
does not run pytest. A file is **ported** when the behaviour it pins is
covered by `engine/core` tests, or by the black-box harness in
`scripts/parity.py`, which drives both cores. A file is **retired** when
it tests a Python-only tool, a packaging step, a web or engine surface the
core crate does not own, or a demo script. Retired is a decision, not a
gap: the reason is the row. **Unported** is a deferral with an owner: the row names
the work item that will port it, and every unported row has one. A deferral
is a disposition (operator decision, WI-1080 review): the behaviour is not in
the Rust core yet, so its test is ported with the feature, not here.

`tests/fixtures/` stays. Both cores and the harness read it.

The Python lint and type gates (`ruff check`, `ruff format --check`, `mypy
--strict` in `.github/workflows/ci.yml`) are retired in favour of
`cargo fmt --check` and `cargo clippy -- -D warnings`. They still run until
the Python package is removed; they come out with it, not before.

The Python floor was `pytest --cov-fail-under=80`. `cargo llvm-cov -p
vogt-core` first measured 79.72% line coverage (WI-1080, at rust-core
77ef27b6), and the gate started at 79%. Since the service ports added
tests, it measures 80.22% (rust-core a5ac4cba), so
`.github/workflows/rust-core.yml` now fails under 80%, the Python floor.
The floor only moves up.

There is no `tests/test_resolve*.py`: Python's resolver has no test file of its
own. Its cases (`WI-n`, slugs, `identity_ref`, and each not-found error) are
pinned by the `application/resolve.rs` tests in the Rust core (WI-1058).

| File | Disposition | Where / why |
|---|---|---|
| test_agent_activity.py | ported | `collectors/agent_activity.rs` |
| test_agent_auth_launch.py | retired | `engine/deploy/agent-auth.sh` launch timing. Shell, not the core crate. |
| test_agent_cli_boot_contract.py | retired | `engine/deploy/agent-cli-install.sh` and `verify-agent-clis.sh`. Engine image boot, not the core. |
| test_agent_clis.py | ported | Agent-CLI report service. The engine owns the mechanism; the core passes the report through. |
| test_agent_ergonomics.py | ported | Registry aliases, the workflow walk, and summary mode in `registry.rs` and the work services. |
| test_audit_query.py | ported | Audit filters in `storage/sqlite/declared.rs` and the audit service. |
| test_auth.py | ported | `auth.rs` and `application/services/auth.rs`. |
| test_benchmark.py | retired | Scale tripwire over the Python process. Not a behaviour assertion. |
| test_board_honesty.py | ported | Board honesty reads in `application/views.rs`. |
| test_board_paging.py | ported | Bounded board pages in `application/views.rs`. |
| test_bootstrap_agent_token.py | ported | Agent-token adoption in `application/services/install.rs`. |
| test_bootstrap_core_token.py | ported | Core-token adoption in `application/services/install.rs`. |
| test_branch_binding.py | ported | `branches.rs` and the work-item branch binding service. |
| test_bridge.py | ported | `adapters/mcp/bridge.rs`. |
| test_bundle_budget.py | retired | PWA first-screen bundle budget. `web/`, not the core. |
| test_cargo_cache.py | retired | `engine/deploy/cargo-cache.sh` (WI-1256, done): Cargo cache survival on redeploy. Shell in the engine image, not the core crate; the Python test keeps driving the script. |
| test_chats.py | ported | Chat operations in the application services, with the engine transport stood in. |
| test_check_engine_spec.py | retired | `scripts/check_engine_spec.py` against the Rust engine routes. Engine contract, not the core. |
| test_check_rust_version.py | retired | `scripts/check_rust_version.py`. A Python lint over the engine toolchain pin. |
| test_check_workflow_policy.py | retired | `scripts/check_workflow_policy.py` over GitHub workflow YAML. CI policy, not product behaviour. |
| test_checks.py | ported | CI-revision reads in the collectors and the project brief. |
| test_ci_watch.py | ported | Watched-ref and deployed-version reads are in `application/deployed.rs`. The bound-branch announce is in `application/ci_watch.rs`: the feed event once per conclusion, staleness, the notice text, and the session nudge (notice, then Enter; none at a permission prompt). `test_a_sweep_announces_and_survives_an_announce_failure` is `a_failed_announce_does_not_fail_the_sweep` in `application/collect.rs`, driven by the `ci_watch::seam` failure switch. |
| test_claude_profile.py | retired | `engine/deploy/claude-profile.sh` and `mcp-bootstrap.sh` (WI-1306, done). Shell in the engine image, not the core crate; the Python test keeps driving the scripts. Named accounts on `session.start` are a sessions concern and are tested with that service. |
| test_cli.py | ported | `adapters/cli.rs`. |
| test_clone.py | ported | Clone service under `application/services`. |
| test_collectors.py | ported | `collectors/` and `application/collect.rs`. |
| test_config.py | ported | Precedence and default policy in `config.rs`. Generated-doc drift stays `scripts/gen_config_docs.py` until that generator is ported. |
| test_connect.py | ported | Connect-document rendering in the application layer. |
| test_contract_adoption.py | ported | Contract opt-in in `application/projects.rs` and `application/contracts.rs`. |
| test_contract_config.py | ported | Contract sourced from configuration, in `config.rs` and `collectors/contract_checker.rs`. |
| test_contracts.py | ported | `collectors/contract_checker.rs`. |
| test_core.py | ported | `core.rs` (ids, time, digests, principals). |
| test_decisions.py | ported | Decision entity and `decision.*` operations (WI-1321, migration 0023): `application/services/decisions_test.rs`. |
| test_delivery.py | ported | `delivery.rs`. |
| test_demo_delivery.py | retired | Browser-only public demo delivery. Not the core. |
| test_diagnostics.py | ported | Instance diagnostics service. |
| test_dispatch.py | unported | Dispatch profiles as data (WI-1323, migration 0024 path). Deferred to WI-1058. |
| test_drift.py | ported | `application/drift.rs`. |
| test_engine_image_core.py | retired | Static checks on `engine/Dockerfile` and `engine/deploy/entrypoint.sh` (WI-1078). Engine image, not the core crate. |
| test_engine_parity.py | retired | Rust engine wire parity. Owned by `engine/server` tests, not `engine/core`. |
| test_fake_agent.py | retired | `scripts/fake-agent` test double for the engine. Not product behaviour. |
| test_forge_accounts.py | ported | `adapters/forge/accounts.rs` and the forge-account service. |
| test_forge_import.py | ported | `forge_ops_test.rs`: the clone, register and consolidate path under the file token; the Forgejo picker, which lists host-qualified repositories under the host's file token; the Forgejo import, which clones the host-qualified URL under that token; the clone under the actor's linked PAT; the display-name override and the write-back policy on an imported project; and the refusals. Cross-core parity of the success paths is not checked (WI-1072 operator decision: in-process only). |
| test_forge_live.py | retired | Live forge network. The port keeps the offline stand-in tests; a live call is not a unit of the core crate. |
| test_forge_module.py | ported | Ported in `forge_module_test.rs`: onboarding is read-only and says what it read, the closed history stays out of `bugs` and `backlog` and is counted as `closed_upstream`, closed observations stay queryable, an unlabelled issue is marked as guessed, the `forge.onboard` audit row keeps its reason (including an unreadable project), a consolidated project briefs with the same count and order as `backlog`, the brief names its declared and observed halves, `by_state` stays declared-only, the Forgejo host is read and an unreadable host names what it can, no repository URL is "not collected", the default policy is `none`, inbound comments are not copied, a `none` policy sends nothing and records why, every ledger row carries its reason, drift raises a state mismatch on a closed issue and accepting it closes the item with provenance, a referenced issue and a PR number in a title are told apart, an adopted link raises one proposal, an observed reopen is automatic, a missing automation toggle names all three facts, and the forge demo runs end to end. The policy table, the absence of a destructive verb and the refusal of a non-additive method are in `adapters/forge/writeback.rs` and `adapters/forge/transport.rs`. The upstream consequence of a write-back is ported (WI-1780): a comment or a transition into a terminal state on an adopted item posts the comment or closes the linked issue when the policy permits, and records a `succeeded` ledger row with its `source_url` (`a_comment_posts_upstream_under_comment_only`, `finishing_an_item_closes_the_linked_issue_under_full`). The forge is called before the declared write transaction opens, and a failed write-back is recorded as `failed` without rolling back the local change (`a_failed_write_back_keeps_the_local_change_and_records_the_failure`). Every Python case has a Rust counterpart. Owner WI-1780. |
| test_forge_provider.py | ported | GitHub and Forgejo provider stand-ins in `adapters/forge/github.rs` and `forgejo.rs`. |
| test_forge_publish.py | unported | Ported in `forge_ops_test.rs`: create-and-push-and-link, the name and visibility choice, the 422 existing-remote refusal (nothing pushed, nothing changed), the non-fast-forward refusal on a real `git push` (the remote history stands), the source gate (empty repository, detached HEAD, branch and revision read, dirty tree by name, the not-a-repository refusal), the already-linked and names-a-repository refusals (pointed at `forge link`), and the no-credential refusal before anything is created. Write-through after publish is checked at the provider, which creates the issue upstream on the published repository. The `work.create` write-through on a linked project is ported (WI-1068) and covered in `upstream_truth_test.rs`. Also not ported: the git argv spy that asserts no `--force` on every path, since `push_branch` runs its fixed argument list internally. Deferred to WI-1072. |
| test_forge_repos.py | ported | Forge repo listing service. |
| test_forge_sync.py | ported | `adapters/forge/sync.rs`. |
| test_forgejo.py | ported | `adapters/forgejo.rs` for the provider, normalisation and writes. The sweep reads (Forgejo issues and PRs reach the backlog, watermarks make the second sweep incremental, a closure upstream leaves the backlog) are in `forge_ops_test.rs`. |
| test_front_door.py | retired | Engine front door (the `/api/vogt` proxy, PTYs, files). `engine/server`, not the core. |
| test_git_story.py | ported | `git_story.rs` and `application/git_story.rs`. |
| test_github.py | ported | `adapters/github.rs`. |
| test_grants.py | ported | `application/grants.rs`. |
| test_handoff.py | unported | Hand-off comment kind and threads (WI-1244, migration 0022). Deferred to WI-1058. |
| test_http.py | ported | `adapters/http/app.rs` (status, etag, validation, openapi). |
| test_import.py | ported | `application/services/project_import_test.rs`: `project.import` through a recording cloner (remote root-path containment for register, create and import; the default root and an explicit root; audited as an import; the duplicate slug refused before any clone; the ways a repository is named and the references refused; the required reason; consolidation under the stand-in forge with no write upstream; the invisible repository refused before a clone; the unconfigured adapter saying "not collected"), and `clone_repository` against a real local git (clone, reuse at parity, the dirty-tree and diverged-branch refusals, a different repository, a checkout with no origin, unrelated files, an empty destination, no token in the clone config and no helper left behind, no credential in a failure message). Not ported: the `subprocess.run` spies (git argv with no `fetch`/`merge`/`rebase`/`pull`/`stash` on a reuse, no `ls-remote` on a fresh clone, no token on any command line, a `which` that finds no git, a git that cannot execute), since `run_git` runs its fixed argument lists internally and has no seam. |
| test_inbox_actors.py | ported | Inbox actor classification in the views service. |
| test_inbox_cost.py | retired | Inbox cost ceiling against the Python process. A performance tripwire, not a behaviour the port re-pins in-process. |
| test_initiative_projection.py | ported | Initiative ranking projection in `application/views.rs`. |
| test_initiative_publish.py | ported | Initiative publish service. |
| test_install.py | retired | Packaging and `uv` checks, plus pytest fixtures around `AppContext`. Install-mode product behaviour is ported in `application/services/install.rs`. The wheel and `uv` assertions are Python-specific and are not re-implemented. |
| test_instance_merge.py | ported | `merge.rs` and the import/merge service. |
| test_lifecycle.py | ported | `application/services/lifecycle.rs`. |
| test_lifecycle_runner.py | retired | `vogt-lifecycle.sh`. A shell runner, not the core crate. |
| test_load_generator.py | retired | `scripts/load.py`. A load generator, not product behaviour. |
| test_logging.py | ported | `observability.rs`. |
| test_login.py | ported | Login routes in `adapters/http/app.rs` and `application/services/auth.rs`. |
| test_m0_demo.py | retired | Milestone demo script. Not a core regression. |
| test_m10_demo.py | retired | Milestone demo script. Not a core regression. |
| test_m1_demo.py | retired | Milestone demo script. Not a core regression. |
| test_m2_demo.py | retired | Milestone demo script. Not a core regression. |
| test_mcp.py | ported | `adapters/mcp/framing.rs` and the MCP HTTP adapter. |
| test_mcp_stdio.py | ported | MCP stdio serving in `main.rs` and `adapters/mcp`. |
| test_metrics.py | unported | Opt-in Prometheus `/metrics` listener (WI-1380). Deferred to WI-1061. |
| test_migration_freeze.py | retired | The pytest copy of the append-only migration freeze. The rule itself is ported in `storage/sqlite/migrator.rs`. |
| test_migrations.py | ported | `storage/sqlite/migrator.rs` (order, checksum, lock, rollback, upgrade-with-data). |
| test_mobile_identity.py | retired | Capacitor application id and the Android manifest. `mobile/`, not the core. |
| test_native_migration.py | ported | Retire-by-marker in `storage/sqlite/declared.rs`. |
| test_notifications.py | ported | Notification reads in the application layer. |
| test_observed_first.py | ported | Promotion and suppression in `application/suppressions.rs` and the views. |
| test_oversight.py | ported | Oversight decisions in `decisions.rs`. |
| test_parity.py | retired | In-process transport parity. Replaced by the black-box harness `scripts/parity.py`. Its four staleness checks (surface membership, exclusion names, exclusion justifications, every shared operation driven) are the harness selftest, ported with the parity chunk. |
| test_parity_normalise.py | retired | Normalisation helpers for the in-process parity script. `scripts/parity.py` owns normalisation now. |
| test_place_metrics_cache.py | unported | Place/metrics badge cache shared across shells (WI-1224). Deferred to WI-1054. |
| test_play_preflight.py | retired | `scripts/play_preflight.py` for the browser suite. `web/`, not the core. |
| test_policy_hook.py | retired | `engine/deploy/policy-hook.sh` and `install-policy`, the PreToolUse hook driven through the real script. Shell, not the core crate; the policy decision it asks the engine for is tested with the engine policy route. |
| test_pr_work_item_edge.py | ported | `implemented_by` from pull requests, in forge sync and the work services. |
| test_product_identity.py | retired | User-facing rename and legacy compatibility names across Python packaging. The Rust binary stays `vogt-core` for the overlap. |
| test_product_version.py | ported | `VERSION` in `main.rs`. `scripts/check_product_version.py` keeps the fallback equal to `pyproject.toml`. |
| test_public_delivery.py | retired | Deploy-file shape. Its 32 cases read the release tags in `pyproject.toml`, the `deploy/` compose files, `deploy/.env.example`, the `Dockerfile`s, the custom-image example and the published docs as text. Two cases run `docker compose config` to render them. None calls the core, so none pins core behaviour, and the deploy files are not a core unit. |
| test_pwa.py | retired | Solid PWA route map. `web/`; the core crate does not embed the bundle. |
| test_ranking.py | ported | Ranking in `decisions.rs`. |
| test_read_pool.py | unported | Pooled read connections (WI-1224). Deferred to WI-1054. |
| test_readonly_mcp_wrappers.py | retired | `engine/deploy/readonly-mcp.sh`. An engine deploy script, not the core. |
| test_registry.py | ported | `registry.rs` (duplicate names, routes, a required reason, scopes). |
| test_release_workflow.py | retired | `.github/workflows/release.yml` digest promotion. CI policy, not product behaviour. |
| test_rust_schemas.py | ported | Derived operation schemas in `application/models.rs` and `registry.rs`, compared with Python's live `registry.dump` by this file, which runs the Rust binary under pytest. |
| test_scheduler.py | ported | In-process collector schedule in `collectors/sweeper.rs` and `application/collect.rs`. |
| test_schema_gate.py | ported | Readiness migration gate in `adapters/http/health.rs` and the migrator. |
| test_serve.py | ported | Serve, health, and MCP-over-HTTP in `main.rs` and `adapters/http`. |
| test_session_bind_work.py | ported | `application/session_work.rs`. |
| test_session_digest_op.py | ported | `application/session_digest.rs`: the six `session.digest` cases (economics carried by the row, absent economics, no window, blocked report, transcript excerpt, unknown session). |
| test_session_drive.py | ported | Session input and screen reads in `application/session_reads.rs` and `delivery.rs`. |
| test_session_economics.py | ported | `core/session_economics.rs`: Claude usage (latest turn, peak is the largest), compaction markers wherever they sit, Klaudia estimate and `compact_boundary`, repeated reads, Codex payload results, junk lines, one-turn wall clock, one turn and its output counted once across content blocks, tool names from `tool_use_id`, `budget_line`, and the shared fixtures (`engine/server/tests/fixtures/economics`: claude, klaudia, codex, klaudia_unicode, claude_gap) with the same numbers. |
| test_session_economics_op.py | ported | `application/session_economics.rs`: metering (with the account name), no transcript, reading off with no roots, the engine reading, and an unknown session. |
| test_session_hibernation.py | ported | Hibernate and wake in `application/services/lifecycle.rs`. |
| test_session_last_reply.py | ported | Transcript reply reads in `adapters/transcripts.rs`. |
| test_session_outcomes.py | ported | `collectors/session_outcomes.rs`. |
| test_session_rename_remove.py | ported | Rename and remove in `application/services/sessions.rs`. |
| test_session_sweep.py | ported | Sweep ordering in `application/session_reads.rs`. |
| test_session_tool_descriptions.py | ported | MCP tool description strings generated from `registry/operations.rs`. |
| test_sessions.py | ported | `application/services/sessions.rs`. |
| test_storage.py | ported | `storage/sqlite/declared.rs` (atomicity, revisions, sessions, tokens). |
| test_test_hooks.py | retired | Deterministic hooks for the in-process golden recorder. The black-box harness records out of process and does not use them. |
| test_transcript_index.py | ported | `engine/core/src/adapters/transcripts.rs` tests (6): lookup results for Claude/Codex ids, missing ids, new files, non-uuid Codex names (by_id now keys Codex by uuid tail or whole stem, as Python), symlinks. Scan-count and TTL assertions unported: Rust has no cached conversation-id index (WI-1413); deferred to WI-1074. |
| test_trust_confirmation.py | ported | Trust-from-last-confirmed in `application/views.rs`. |
| test_unlinked_surfaces.py | ported | Unlinked-project honesty in `application/projects.rs` and the views. |
| test_upgrade_data_volume.py | retired | Compose data-volume upgrade smoke. Deployment, not a core unit test. The migrator's upgrade-with-data cases cover the product half. |
| test_upstream_truth.py | ported | Ported in `upstream_truth_test.rs`: a vogt-only change on an adopted item sends nothing, the unlinked refusal names both ways forward, a local-only item on an unlinked project comments and closes with nothing sent, a projectless item moves, each upstream issue counts once in the backlog and the board (adopted first), a relation between upstream issues is refused with the reason, `forge.link` refuses a project with no repository URL and is audited, a policy-`none` create on a linked project refuses and names every gate, and `local_only` creates work under `none` and on an unlinked project. The create and update write-throughs are ported (WI-1068): a create opens the issue and keys the overlay and ledger row by its subject, a failed create commits nothing, priority and effort on an upstream item are overlay-only, a label add writes through while a removal is refused as append-only, and title and body edits are refused as upstream truth. The comment, close and reopen write-throughs (WI-1780) are ported: `closing_and_reopening_write_through`, `an_open_to_open_move_on_an_upstream_item_sends_nothing`, `a_comment_posts_upstream_and_stores_no_local_row`, `a_handoff_comment_on_an_upstream_item_is_refused`, `a_failed_comment_and_close_raise_and_change_nothing`; a walk checks every hop's policy before moving anything (`a_walk_refused_mid_way_moves_nothing`). The board gaps (WI-1778) are fixed: an upstream-only issue is on the board once (`board_lists_an_upstream_only_issue_once`) and the cell cursor continues under its snapshot (`the_board_pages_with_stable_cursors`). WI-1779 was a misreading, now closed by a test: Python's `adopt` inserts a native row, so title, body and label-removal edits on an adopted item are local edits in both cores (`title_body_and_label_removal_on_an_adopted_item_are_local_edits`); the upstream-truth refusals apply to the un-adopted subject. Filed local-only because the vogt write-back policy is none. The import-with-consolidation link is `forge_import_clones_registers_and_consolidates_under_the_file_token`; the label refusal on an unlinked project is `a_label_edit_on_an_unlinked_project_is_refused`. Owner WI-1780. |
| test_view_paging.py | ported | Ranked backlog and bugs paging in `application/views.rs`. |
| test_voice_capabilities.py | retired | Voice sidecar policy. `voice/` is a separate workspace. |
| test_work_get_many.py | ported | `work.get_many` batched chip read (WI-1224). Owner WI-1068. |
| test_workflow.py | ported | Workflow machines in `decisions.rs`. |
| test_workspace_disk.py | unported | Workspace-volume row on engine status and sweep (WI-1304). Deferred to WI-1066. |
| test_writes.py | ported | `application/writes_test.rs`. |
