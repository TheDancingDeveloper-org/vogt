
# HTTP test map: Python suites to Rust counterparts
Totals across the four suites: covered 76, partial 0, gap 0, not core 1 (of 77).


Generated from the Python suites named in WI-1061 Done means. Status:

- **covered**: a Rust test asserts the same behaviour.
- **partial**: a Rust test covers part of it; the rest is listed.
- **gap**: no Rust counterpart yet. Not ported in this slice.
- **not core**: the behaviour belongs to another port slice; named in the row.

`tests/test_auth.py` has no test that uses a client or the HTTP surface (checked:
no `TestClient` or `build_server`), so it contributes no HTTP tests. Its service
tests are mapped by the rows below where they cover the same behaviour.

## test_serve.py (22 tests: covered 21, partial 0, gap 0, not core 1)

| Python test | status | Rust counterpart |
|---|---|---|
| `test_health_and_version_need_no_credential` | covered | health::tests::version_and_connection_info_match_python (probes sit outside the auth gate) |
| `test_readiness_reports_why_it_is_not_ready` | covered | health::tests::a_probe_on_an_empty_store_says_why_it_is_not_ready (names `vogt migrate`, not `vogt init`, the Rust build's step) |
| `test_connection_info_is_generated_by_the_server` | covered | health::tests::version_and_connection_info_match_python |
| `test_everything_answers_on_the_one_port` | covered | tests/serve_cli.rs::every_surface_answers_on_the_one_port (the built binary, one listener) |
| `test_the_api_refuses_an_unauthenticated_caller` | covered | app::tests::no_token_is_refused_before_the_operation_runs |
| `test_a_read_token_reads` | covered | app::tests::a_read_token_reads |
| `test_a_read_token_cannot_write` | covered | app::tests::a_read_token_cannot_write |
| `test_a_write_token_writes` | covered | app::tests::a_write_token_writes |
| `test_the_write_is_attributed_to_the_token_holder` | covered | app::tests::the_write_is_attributed_to_the_token_holder |
| `test_a_garbage_token_is_401_not_500` | covered | app::tests::a_bad_token_is_refused_too (asserts 401, unauthenticated) |
| `test_the_loopback_surface_needs_no_token` | covered | app::tests::no_auth_serves_a_ported_operation |
| `test_mcp_initialize_over_http` | covered | mcp::tests::initialize_over_http_names_the_server |
| `test_an_unknown_protocol_version_negotiates_down` | covered | mcp::tests::an_unknown_protocol_version_negotiates_down_over_http |
| `test_ungranted_tools_are_invisible_not_erroring` | covered | mcp::tests::ungranted_tools_are_invisible_not_erroring |
| `test_calling_an_ungranted_tool_is_refused_and_recorded` | covered | mcp::tests::calling_an_ungranted_tool_over_http_is_refused_and_recorded |
| `test_local_only_tools_are_not_callable_over_http` | covered | app::tests::a_local_only_operation_is_not_on_http; framing::tests::a_local_only_tool_cannot_be_called_either |
| `test_a_granted_tool_call_works_over_http` | covered | mcp::tests::a_granted_tool_call_works_over_http |
| `test_a_listen_address_is_required` | covered | tests/serve_cli.rs::serve_refuses_to_start_without_a_listen_address |
| `test_tls_needs_both_halves` | covered | main::tests::one_flag_without_the_other_is_refused |
| `test_a_missing_certificate_is_named` | covered | main::tests::a_missing_file_is_named |
| `test_the_scheme_follows_the_certificate` | covered | app::tests::the_forwarded_scheme_is_trusted_only_from_loopback (listener scheme fallback) |
| `test_new_voice_writes_over_mcp_enforce_scope_reason_and_persist_approver_audit` | not core | tests the engine's voice approval path, not the core HTTP adapter; owned by the engine port |

## test_front_door.py (5 tests: covered 5, partial 0, gap 0)

| Python test | status | Rust counterpart |
|---|---|---|
| `test_a_write_through_the_front_door_is_audited_as_the_caller` | covered | engine/server/tests/front_door.rs::a_write_through_the_front_door_is_audited_as_the_caller (boots the real `vogt-core`, reads `vogt-core audit list`) |
| `test_the_break_glass_token_writes_as_the_stack_secrets_actor` | covered | engine/server/tests/front_door.rs::the_break_glass_token_writes_as_the_stack_secrets_actor (real `vogt-core` audit row for `agent:vogt-engine`) |
| `test_the_front_doors_gate_and_the_cores_gate_both_stand` | covered | engine/server/tests/front_door.rs::the_front_doors_gate_and_the_cores_gate_both_stand |
| `test_an_unknown_credential_is_refused_at_the_door` | covered | engine/server/tests/front_door.rs::an_unknown_credential_is_refused_at_the_door |
| `test_a_person_signs_in_with_a_password_and_writes_as_themselves` | covered | engine/server/tests/front_door.rs::a_person_signs_in_with_a_password_and_writes_as_themselves |

## test_login.py (28 tests: covered 28, partial 0, gap 0)

| Python test | status | Rust counterpart |
|---|---|---|
| `test_a_password_verifies_against_its_hash_and_nothing_else_does` | covered | auth::tests::a_password_verifies_against_a_python_stored_hash; services::auth::tests::a_password_verifies_against_its_hash_and_nothing_else_does |
| `test_two_hashes_of_one_password_differ` | covered | services::auth::tests::two_hashes_of_one_password_differ |
| `test_a_short_password_is_refused` | covered | services::auth::tests::a_short_password_is_refused |
| `test_usernames_are_folded_and_validated` | covered | auth::tests::a_username_is_folded_and_bounded |
| `test_creating_a_user_creates_a_human_actor_and_a_login` | covered | services::auth::tests::creating_a_user_creates_a_human_actor_and_a_login |
| `test_a_user_result_never_carries_the_hash` | covered | services::auth::tests::a_user_result_never_carries_the_hash |
| `test_a_duplicate_username_is_a_conflict` | covered | services::auth::tests::a_duplicate_username_is_a_conflict |
| `test_a_bad_username_or_scope_is_an_invalid_request` | covered | services::auth::tests::a_bad_username_or_scope_is_an_invalid_request; application::services::auth::tests::adopt_refuses_a_short_secret_and_scopes_reject_the_unknown |
| `test_an_agent_cannot_hold_a_password` | covered | services::auth::tests::an_agent_cannot_hold_a_password |
| `test_a_login_mints_a_session_bound_to_the_person` | covered | services::auth::tests::a_login_mints_a_session_bound_to_the_person |
| `test_the_login_is_audited_to_the_person_without_the_secret` | covered | services::auth::tests::the_login_is_audited_to_the_person_without_the_secret |
| `test_a_wrong_password_and_an_unknown_user_read_the_same` | covered | application::services::auth::tests::an_unknown_user_and_a_disabled_user_read_the_same; app::tests::login_refuses_an_unknown_user_with_the_one_sentence |
| `test_a_username_that_keeps_failing_is_throttled` | covered | application::services::auth::tests::a_username_that_keeps_failing_is_locked_out |
| `test_a_disabled_actor_cannot_log_in` | covered | application::services::auth::tests::an_unknown_user_and_a_disabled_user_read_the_same |
| `test_logout_revokes_the_session_it_arrived_with` | covered | services::auth::tests::logout_revokes_the_session_it_arrived_with |
| `test_logout_with_no_token_behind_it_is_an_honest_no` | covered | services::auth::tests::logout_with_no_token_behind_it_is_an_honest_no |
| `test_whoami_reports_the_authenticated_identity_and_effective_scopes` | covered | services::auth::tests::whoami_reports_the_authenticated_identity_and_effective_scopes |
| `test_setting_a_password_ends_every_session_and_takes_effect_at_once` | covered | application::services::auth::tests::setting_a_password_revokes_the_sessions |
| `test_setting_a_password_can_keep_sessions_and_change_scopes` | covered | services::auth::tests::setting_a_password_can_keep_sessions_and_change_scopes |
| `test_removing_a_user_revokes_sessions_and_keeps_the_actor` | covered | services::auth::tests::removing_a_user_keeps_the_actor (now also asserts the session is revoked) |
| `test_the_bootstrap_can_create_a_login_and_hands_back_a_session` | covered | services::install::tests::the_bootstrap_can_create_a_login_and_hands_back_a_session |
| `test_the_bootstrap_without_a_password_is_the_headless_shape` | covered | application::services::install::tests::a_headless_bootstrap_mints_an_api_token |
| `test_the_bootstrap_refuses_a_username_without_a_password` | covered | services::install::tests::the_bootstrap_refuses_a_username_without_a_password |
| `test_login_needs_no_credential_and_the_session_it_mints_works` | covered | app::tests::a_login_needs_no_credential_and_the_session_it_mints_works |
| `test_a_wrong_password_over_http_is_401_and_a_throttle_is_429` | covered | app::tests::a_wrong_password_over_http_is_401_and_a_throttle_is_429 |
| `test_a_session_in_regular_use_slides_and_is_never_signed_out` | covered | services::auth::tests::a_session_in_regular_use_slides_and_is_never_signed_out |
| `test_an_unused_session_still_expires_and_a_revoked_one_is_not_revived` | covered | services::auth::tests::an_unused_session_still_expires_and_a_revoked_one_is_not_revived |
| `test_only_a_login_session_slides` | covered | services::auth::tests::only_a_login_session_slides |

## test_install.py (22 tests: covered 22, partial 0, gap 0)

| Python test | status | Rust counterpart |
|---|---|---|
| `test_a_fresh_instance_is_in_install_mode` | covered | app::tests::install_status_is_public_and_open_on_an_empty_store |
| `test_bootstrap_names_the_operator_and_mints_an_admin_token` | covered | app::tests::bootstrap_names_the_first_operator_exactly_once |
| `test_disabling_the_bootstrap_refuses_it_and_reports_closed` | covered | application::services::install::tests::a_disabled_bootstrap_says_it_is_disabled |
| `test_bootstrap_closes_install_mode` | covered | application::services::install::tests::the_first_bootstrap_closes_the_door |
| `test_a_persons_token_closes_install_mode` | covered | application::services::install::tests::the_door_follows_people_not_tokens |
| `test_an_agents_token_leaves_install_mode_open` | covered | application::services::install::tests::the_door_follows_people_not_tokens |
| `test_a_password_login_closes_install_mode` | covered | services::install::tests::a_password_login_closes_install_mode |
| `test_revoking_every_token_does_not_reopen_install_mode` | covered | application::services::install::tests::a_latch_or_a_revoked_person_token_closes_the_door |
| `test_the_bootstrap_is_audited_and_attributed_to_the_new_actor` | covered | services::install::tests::the_bootstrap_is_audited_and_attributed_to_the_new_actor |
| `test_a_failed_bootstrap_leaves_no_actor_behind` | covered | services::install::tests::a_failed_bootstrap_leaves_no_actor_behind |
| `test_an_explicit_identity_ref_is_honoured` | covered | services::install::tests::an_explicit_identity_ref_is_honoured |
| `test_an_unsluggable_display_name_is_refused` | covered | services::install::tests::an_unsluggable_display_name_is_refused |
| `test_install_status_needs_no_credential` | covered | app::tests::install_status_is_public_and_open_on_an_empty_store |
| `test_the_bootstrap_issues_a_working_token_over_http` | covered | app::tests::the_bootstrap_issues_a_working_token_over_http |
| `test_a_closed_bootstrap_refuses_with_a_named_reason` | covered | app::tests::a_closed_bootstrap_refuses_with_a_named_reason |
| `test_the_bootstrap_validates_its_body` | covered | app::tests::a_bad_bootstrap_body_is_rejected_with_no_side_effects |
| `test_the_adopted_core_token_leaves_install_mode_open` | covered | services::install::tests::the_adopted_core_token_leaves_install_mode_open (core-token row seeded in init's shape) |
| `test_a_quick_start_reaches_a_first_operator_end_to_end` | covered | tests/serve_cli.rs::a_quick_start_reaches_a_first_operator_end_to_end |
| `test_the_cli_fallback_also_closes_a_quick_start` | covered | tests/serve_cli.rs::the_cli_fallback_also_closes_a_quick_start |
| `test_an_upgraded_instance_operated_without_a_person_stays_closed` | covered | services::install::tests::an_upgraded_instance_operated_without_a_person_stays_closed |
| `test_an_upgraded_store_with_no_tokens_is_still_a_fresh_install` | covered | services::install::tests::an_upgraded_store_with_no_tokens_is_still_a_fresh_install |
| `test_removing_the_only_operator_does_not_reopen_install_mode` | covered | services::install::tests::removing_the_only_operator_does_not_reopen_install_mode |


## Open items

WI-1742 ported the 39 gap and 13 partial rows as Rust tests. WI-1747 closed the
last two partial rows, both in `test_front_door.py`, by booting the real
`vogt-core` binary inside `engine/server/tests/front_door.rs`. Those two tests
write through the engine's door and read the audit table back with
`vogt-core audit list`, as the Python test does.

The other front-door tests still run against a stand-in core. The door itself
is real in all of them: the bearer gate, the capability gate, identity caching,
credential forwarding and logout eviction. The real-core tests need
`cargo build -p vogt-core` first and fail, rather than skip, when the binary is
missing.
