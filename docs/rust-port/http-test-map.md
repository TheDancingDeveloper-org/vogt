
# HTTP test map: Python suites to Rust counterparts
Totals across the four suites: covered 24, partial 13, gap 39, not core 1 (of 77).


Generated from the Python suites named in WI-1061 Done means. Status:

- **covered**: a Rust test asserts the same behaviour.
- **partial**: a Rust test covers part of it; the rest is listed.
- **gap**: no Rust counterpart yet. Not ported in this slice.
- **not core**: the behaviour belongs to another port slice; named in the row.

`tests/test_auth.py` has no test that uses a client or the HTTP surface (checked:
no `TestClient` or `build_server`), so it contributes no HTTP tests. Its service
tests are mapped by the rows below where they cover the same behaviour.

## test_serve.py (22 tests: covered 9, partial 3, gap 9, not core 1)

| Python test | status | Rust counterpart |
|---|---|---|
| `test_health_and_version_need_no_credential` | covered | health::tests::version_and_connection_info_match_python (probes sit outside the auth gate) |
| `test_readiness_reports_why_it_is_not_ready` | gap | health::tests cover the ready body shape only; the not-ready reason is untested |
| `test_connection_info_is_generated_by_the_server` | covered | health::tests::version_and_connection_info_match_python |
| `test_everything_answers_on_the_one_port` | gap | none; main.rs merges the routers, but no test serves them together |
| `test_the_api_refuses_an_unauthenticated_caller` | covered | app::tests::no_token_is_refused_before_the_operation_runs |
| `test_a_read_token_reads` | gap | needs a read-scoped token minted over HTTP |
| `test_a_read_token_cannot_write` | gap | needs a read-scoped token minted over HTTP |
| `test_a_write_token_writes` | gap | needs a write-scoped token minted over HTTP |
| `test_the_write_is_attributed_to_the_token_holder` | gap | needs a token minted over HTTP; attribution is in auth_gate::tests::an_authenticated_grant_keeps_its_actor at unit level |
| `test_a_garbage_token_is_401_not_500` | covered | app::tests::a_bad_token_is_refused_too (asserts 401, unauthenticated) |
| `test_the_loopback_surface_needs_no_token` | covered | app::tests::no_auth_serves_a_ported_operation |
| `test_mcp_initialize_over_http` | partial | framing::tests::initialize_answers_with_the_negotiated_version; no HTTP /mcp test |
| `test_an_unknown_protocol_version_negotiates_down` | partial | framing::tests::an_unknown_version_negotiates_down_instead_of_refusing; no HTTP test |
| `test_ungranted_tools_are_invisible_not_erroring` | gap | framing::tests::tools_list_comes_from_the_registry_and_hides_local_only covers local-only, not grants |
| `test_calling_an_ungranted_tool_is_refused_and_recorded` | gap | http::mcp::tests::an_anonymous_tool_call_is_refused_and_records_nothing covers anonymous only |
| `test_local_only_tools_are_not_callable_over_http` | covered | app::tests::a_local_only_operation_is_not_on_http; framing::tests::a_local_only_tool_cannot_be_called_either |
| `test_a_granted_tool_call_works_over_http` | partial | app::tests::no_auth_serves_a_ported_operation (HTTP); framing::tests::a_success_carries_the_whole_result_as_compact_text |
| `test_a_listen_address_is_required` | gap | clap requires --host and --port; no test asserts it |
| `test_tls_needs_both_halves` | covered | main::tests::one_flag_without_the_other_is_refused |
| `test_a_missing_certificate_is_named` | covered | main::tests::a_missing_file_is_named |
| `test_the_scheme_follows_the_certificate` | covered | app::tests::the_forwarded_scheme_is_trusted_only_from_loopback (listener scheme fallback) |
| `test_new_voice_writes_over_mcp_enforce_scope_reason_and_persist_approver_audit` | not core | tests the engine's voice approval path, not the core HTTP adapter; owned by the engine port |

## test_front_door.py (5 tests: gap 5)

| Python test | status | Rust counterpart |
|---|---|---|
| `test_a_write_through_the_front_door_is_audited_as_the_caller` | gap | the front door is the engine's /api/vogt door (engine/server); no Rust counterpart in this slice |
| `test_the_break_glass_token_writes_as_the_stack_secrets_actor` | gap | the front door is the engine's /api/vogt door (engine/server); no Rust counterpart in this slice |
| `test_the_front_doors_gate_and_the_cores_gate_both_stand` | gap | the front door is the engine's /api/vogt door (engine/server); no Rust counterpart in this slice |
| `test_an_unknown_credential_is_refused_at_the_door` | gap | the front door is the engine's /api/vogt door (engine/server); no Rust counterpart in this slice |
| `test_a_person_signs_in_with_a_password_and_writes_as_themselves` | gap | the front door is the engine's /api/vogt door (engine/server); no Rust counterpart in this slice |

## test_login.py (28 tests: covered 6, partial 7, gap 15)

| Python test | status | Rust counterpart |
|---|---|---|
| `test_a_password_verifies_against_its_hash_and_nothing_else_does` | partial | auth::tests::a_password_verifies_against_a_python_stored_hash (positive case only) |
| `test_two_hashes_of_one_password_differ` | gap | no salt-uniqueness test |
| `test_a_short_password_is_refused` | gap | no test |
| `test_usernames_are_folded_and_validated` | covered | auth::tests::a_username_is_folded_and_bounded |
| `test_creating_a_user_creates_a_human_actor_and_a_login` | gap | no test |
| `test_a_user_result_never_carries_the_hash` | gap | no test |
| `test_a_duplicate_username_is_a_conflict` | gap | no test |
| `test_a_bad_username_or_scope_is_an_invalid_request` | partial | auth::tests::adopt_refuses_a_short_secret_and_scopes_reject_the_unknown (scopes only) |
| `test_an_agent_cannot_hold_a_password` | gap | no test |
| `test_a_login_mints_a_session_bound_to_the_person` | partial | application::services::auth::tests::a_wrong_password_is_recorded_and_the_right_one_signs_in (session binding not asserted) |
| `test_the_login_is_audited_to_the_person_without_the_secret` | gap | no test |
| `test_a_wrong_password_and_an_unknown_user_read_the_same` | covered | application::services::auth::tests::an_unknown_user_and_a_disabled_user_read_the_same; app::tests::login_refuses_an_unknown_user_with_the_one_sentence |
| `test_a_username_that_keeps_failing_is_throttled` | covered | application::services::auth::tests::a_username_that_keeps_failing_is_locked_out |
| `test_a_disabled_actor_cannot_log_in` | covered | application::services::auth::tests::an_unknown_user_and_a_disabled_user_read_the_same |
| `test_logout_revokes_the_session_it_arrived_with` | gap | no test |
| `test_logout_with_no_token_behind_it_is_an_honest_no` | gap | no test |
| `test_whoami_reports_the_authenticated_identity_and_effective_scopes` | gap | no test |
| `test_setting_a_password_ends_every_session_and_takes_effect_at_once` | covered | application::services::auth::tests::setting_a_password_revokes_the_sessions |
| `test_setting_a_password_can_keep_sessions_and_change_scopes` | gap | no test |
| `test_removing_a_user_revokes_sessions_and_keeps_the_actor` | partial | application::services::auth::tests::removing_a_user_keeps_the_actor (session revocation not asserted) |
| `test_the_bootstrap_can_create_a_login_and_hands_back_a_session` | gap | no test |
| `test_the_bootstrap_without_a_password_is_the_headless_shape` | covered | application::services::install::tests::a_headless_bootstrap_mints_an_api_token |
| `test_the_bootstrap_refuses_a_username_without_a_password` | gap | no test |
| `test_login_needs_no_credential_and_the_session_it_mints_works` | partial | app::tests::login_is_public_and_validates_before_anything_else (no-credential part; the minted session is not used) |
| `test_a_wrong_password_over_http_is_401_and_a_throttle_is_429` | partial | app::tests::login_refuses_an_unknown_user_with_the_one_sentence (401 only); the 429 is not asserted over HTTP |
| `test_a_session_in_regular_use_slides_and_is_never_signed_out` | partial | auth::tests::only_a_session_past_half_its_life_slides (the threshold, not the sliding path) |
| `test_an_unused_session_still_expires_and_a_revoked_one_is_not_revived` | gap | no test |
| `test_only_a_login_session_slides` | gap | no test |

## test_install.py (22 tests: covered 9, partial 3, gap 10)

| Python test | status | Rust counterpart |
|---|---|---|
| `test_a_fresh_instance_is_in_install_mode` | covered | app::tests::install_status_is_public_and_open_on_an_empty_store |
| `test_bootstrap_names_the_operator_and_mints_an_admin_token` | covered | app::tests::bootstrap_names_the_first_operator_exactly_once |
| `test_disabling_the_bootstrap_refuses_it_and_reports_closed` | covered | application::services::install::tests::a_disabled_bootstrap_says_it_is_disabled |
| `test_bootstrap_closes_install_mode` | covered | application::services::install::tests::the_first_bootstrap_closes_the_door |
| `test_a_persons_token_closes_install_mode` | covered | application::services::install::tests::the_door_follows_people_not_tokens |
| `test_an_agents_token_leaves_install_mode_open` | covered | application::services::install::tests::the_door_follows_people_not_tokens |
| `test_a_password_login_closes_install_mode` | gap | no test |
| `test_revoking_every_token_does_not_reopen_install_mode` | covered | application::services::install::tests::a_latch_or_a_revoked_person_token_closes_the_door |
| `test_the_bootstrap_is_audited_and_attributed_to_the_new_actor` | gap | no test |
| `test_a_failed_bootstrap_leaves_no_actor_behind` | partial | application::services::install::tests::two_bootstraps_at_once_mint_one_token (single token; no actor rollback asserted) |
| `test_an_explicit_identity_ref_is_honoured` | gap | no test |
| `test_an_unsluggable_display_name_is_refused` | gap | no test |
| `test_install_status_needs_no_credential` | covered | app::tests::install_status_is_public_and_open_on_an_empty_store |
| `test_the_bootstrap_issues_a_working_token_over_http` | partial | app::tests::bootstrap_names_the_first_operator_exactly_once (mint asserted; the token is not used) |
| `test_a_closed_bootstrap_refuses_with_a_named_reason` | gap | no HTTP test of the closed reason |
| `test_the_bootstrap_validates_its_body` | covered | app::tests::a_bad_bootstrap_body_is_rejected_with_no_side_effects |
| `test_the_adopted_core_token_leaves_install_mode_open` | gap | no test |
| `test_a_quick_start_reaches_a_first_operator_end_to_end` | gap | no test; quick start spans the CLI |
| `test_the_cli_fallback_also_closes_a_quick_start` | gap | no test; the CLI user-create path |
| `test_an_upgraded_instance_operated_without_a_person_stays_closed` | partial | application::services::install::tests::a_latch_or_a_revoked_person_token_closes_the_door (latch only) |
| `test_an_upgraded_store_with_no_tokens_is_still_a_fresh_install` | gap | no test |
| `test_removing_the_only_operator_does_not_reopen_install_mode` | gap | no test |

