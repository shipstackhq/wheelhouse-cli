# Test port checklist

Every test case in `daemon/test` and `cli/test`, with the Rust test that replaces it. Fill the right column as work lands
(`crate::module::test_name`); `-` means not ported yet. Do not delete a row: if a case no longer applies, write `n/a: <why>`.

## Decisions that shape the port

- **HTTP and SSE stack: async `tokio` + `reqwest`** (rustls, platform verifier). Wheelhouse decision #1, accepted. The daemon runs on consumer machines and server-grade VMs and orchestrates many parallel flows, so async is the default. The ureq + threads spike is kept for reference and is not part of the product.
- Binary size trade-off: the tokio + reqwest spike is about 235 KB larger than the ureq spike (1,587,136 B vs 1,353,536 B, macOS arm64, release). Size budgets follow CI measurements on all three targets.

## daemon/test/crypto.test.mjs
| Node test | Rust counterpart |
|---|---|
| canonical JSON is the server's: keys sorted at every level, undefined dropped | wh-proto `canonical::tests`, `tests/node_compat.rs::canonical_bytes_match_node` |
| a signature over the canonical form verifies whatever the key order it arrives in | wh-proto `sign::tests::sign_verify_any_key_order`, `node_compat::server_signatures_verify_and_keys_round_trip` |
| the assertion is an EdDSA JWT for the token endpoint, signed by the device key, two minutes long | wh-proto `sign::tests::assertion_shape`, `node_compat::assertion_is_byte_identical_to_nodes` |

## daemon/test/sse.test.mjs
| Node test | Rust counterpart |
|---|---|
| messages with id, event and data, comments skipped, text cut anywhere | wh-proto `sse::tests::messages_comments_and_any_cut` |
| CRLF line ends, and a message without an event is a plain message | wh-proto `sse::tests::crlf_and_plain_message` |

## daemon/test/config.test.mjs
| Node test | Rust counterpart |
|---|---|
| only settings that differ from the defaults are saved, so new defaults reach an existing machine | wh-core `config::file_tests::only_settings_that_differ_from_the_defaults_are_saved` (also `unknown_keys_survive_a_save`, `a_saved_runner_is_merged_over_the_defaults`) |

## daemon/test/connection.test.mjs
| Node test | Rust counterpart |
|---|---|
| a request that is never answered is given up on instead of hanging | wh-core `connection::tests::a_request_that_is_never_answered_is_given_up_on_instead_of_hanging` |
| a stream that goes quiet is dropped with a clear error, so the daemon reconnects | wh-core `connection::tests::a_stream_that_goes_quiet_is_dropped_with_a_clear_error_so_the_daemon_reconnects` |
| a stream that keeps receiving keep-alives stays open, and the caller can still end it | wh-core `connection::tests::a_stream_that_keeps_receiving_keep_alives_stays_open_and_the_caller_can_still_end_it` (partial: keep-alives arrive in one burst, not spaced in time; the caller ends it by dropping the stream) |

## daemon/test/environment.test.mjs
| Node test | Rust counterpart |
|---|---|
| PATH entries merge in order without duplicates | wh-core `environment::tests::path_entries_merge_in_order_without_duplicates` |
| claude is found on a PATH, by an explicit path, or not at all | wh-core `environment::tests::claude_is_found_on_a_path_by_an_explicit_path_or_not_at_all` |
| the login shell's PATH can be read, and is a real PATH | wh-core `environment::tests::the_login_shells_path_can_be_read_and_is_a_real_path` |

## daemon/test/runner.test.mjs
| Node test | Rust counterpart |
|---|---|
| the prompt carries the issue and the rules of the run | wh-core `tests/runner_golden.rs::prompts_match_node` (`plain`) |
| a continuation tells the agent about the earlier attempt and that the branch already has its commits | wh-core `tests/runner_golden.rs::prompts_match_node` (`continuation*`) |
| the run is headless and fenced: restricted, no MCP, no hooks, prompts refused, push denied | wh-core `tests/runner_golden.rs::claude_arguments_match_node` (`plain`); the spawn-time checks come with the runner process |
| a run with a key reads Wheelhouse through a private MCP config file that is gone afterwards | wh-core `claude::tests::a_run_with_a_key_reads_wheelhouse_through_a_private_mcp_config_file_that_is_gone_afterwards` (the flags are also covered by `tests/runner_golden.rs::claude_arguments_match_node`) |
| a delegated run uses its agent's definition, and is told to check done and ask | wh-core `tests/runner_golden.rs::claude_arguments_match_node` (`effortAndAgent`) and `prompts_match_node` (`agent`) |
| an answered question is handed to the continuing run | wh-core `tests/runner_golden.rs::prompts_match_node` (`continuationAnswered`) |
| the offer's effort is passed to Claude, and left out when there is none | wh-core `tests/runner_golden.rs::claude_arguments_match_node` (`effortAndAgent`, `plain`) |
| a review reads the pull request, may not edit, and is told to give a verdict | wh-core `tests/runner_golden.rs::prompts_match_node` (`review`) and `claude_arguments_match_node` (`review`) |
| a delegated run is asked for learnings; LEARNING lines leave the summary and become a list | wh-core `tests/runner_golden.rs::stream_lines_match_node` (learnings) and `prompts_match_node` (`agent`) |
| stream-json becomes short progress events | wh-core `tests/runner_golden.rs::stream_lines_match_node` |
| runs the CLI in the worktree and reports its result | wh-core `claude::tests::runs_the_cli_in_the_worktree_and_reports_its_result` |
| an abort stops the agent and its children | wh-core `claude::tests::an_abort_stops_the_agent_and_its_children` (also checks the agent's child process is gone) |
| a missing CLI is a clear failure | wh-core `claude::tests::a_missing_cli_is_a_clear_failure` |
| a chat run is read-only: file tools and read-only git, no MCP, no hooks, partial messages, the prompt as typed | wh-core `chat::tests::a_chat_run_is_read_only_file_tools_and_read_only_git_no_mcp_no_hooks_partial_messages_the_prompt_as_typed` (and the golden `chat_arguments_match_node`) |
| the chat parser: text deltas, tool calls, and the result; the full text only when no deltas came | wh-core `chat::tests::the_chat_parser_text_deltas_tool_calls_and_the_result_the_full_text_only_when_no_deltas_came` (and the golden `chat_parser_matches_node`) |
| runs a chat message and reports deltas, tools and the whole answer; an abort stops it | wh-core `chat::tests::process::runs_a_chat_message_and_reports_deltas_tools_and_the_whole_answer_an_abort_stops_it` |
| chat with a key: MCP config in a 0600 file (not argv), strict config kept, mcp__wheelhouse allowed, rules switch; without one, none of it | wh-core `chat::tests::chat_with_a_key_strict_config_kept_mcp_wheelhouse_allowed_rules_switch_without_one_none_of_it` (and the golden `chat_arguments_match_node`, `chat_rules_match_node`) |
| chat rules offer Wheelhouse writes only when mcp.write is true | wh-core `chat::tests::chat_rules_offer_wheelhouse_writes_only_when_mcp_write_is_true` (and the golden `chat_rules_match_node`) |
| write tool summaries: create_issue shows the title, comment the issue | wh-core `chat::tests::write_tool_summaries_create_issue_shows_the_title_comment_the_issue` |
| the MCP tool summary is short: 'wheelhouse: get_issue WH-42' | wh-core `tests/runner_golden.rs::tool_summaries_match_node` (`mcp`) |
| a keyed chat run writes the config 0600 outside cwd, keeps the token out of argv and events, and deletes it after | wh-core `chat::tests::process::a_keyed_chat_run_writes_the_config_0600_outside_cwd_keeps_the_token_out_of_argv_and_events_and_deletes_it_after` |
| scrub: paths read relative to the run's checkout, the rest of home as ~ | wh-core `tests/runner_golden.rs::scrub_matches_node` |

## daemon/test/supervisor.test.mjs
| Node test | Rust counterpart |
|---|---|
| a verified offer is accepted, run, and finished with what the run reported | wh-core `supervisor::tests::a_verified_offer_is_accepted_run_and_finished_with_what_the_run_reported` |
| a run's learnings go with its finish | wh-core `supervisor::tests::a_runs_learnings_go_with_its_finish` |
| a review commits nothing and pushes nothing, and still finishes with what it said | wh-core `supervisor::tests::a_review_commits_nothing_and_pushes_nothing_and_still_finishes_with_what_it_said` |
| unverifiable messages are dropped without a word to the server | wh-core `supervisor::tests::unverifiable_messages_are_dropped_without_a_word_to_the_server` |
| never more than the pool size at once; the next offer is refused for want of a slot | wh-core `supervisor::tests::never_more_than_the_pool_size_at_once_the_next_offer_is_refused_for_want_of_a_slot` |
| a settings message changes the pool size | wh-core `supervisor::tests::a_settings_message_changes_the_pool_size` |
| a signed cancel stops the run; it is reported canceled, and the work still pushed | wh-core `supervisor::tests::a_signed_cancel_stops_the_run_it_is_reported_canceled_and_the_work_still_pushed` |
| a failing step fails the job with the reason, and frees the slot | wh-core `supervisor::tests::a_failing_step_fails_the_job_with_the_reason_and_frees_the_slot` |
| a run that hits the usage limit pauses new offers | wh-core `supervisor::tests::a_run_that_hits_the_usage_limit_pauses_new_offers` |
| a Stop from Wheelhouse says it was requested; the daemon going down says that instead | wh-core `supervisor::tests::a_stop_from_wheelhouse_says_it_was_requested_the_daemon_going_down_says_that_instead` |
| shutdown cancels what is running and waits for it to report | wh-core `supervisor::tests::shutdown_cancels_what_is_running_and_waits_for_it_to_report` |
| a run whose branch a newer run took over stops without committing or pushing, and keeps its worktree | wh-core `supervisor::tests::a_run_whose_branch_a_newer_run_took_over_stops_without_committing_or_pushing_and_keeps_its_worktree` |
| a finish that cannot get through is kept with the job and listed with it until it is sent | wh-core `supervisor::tests::a_finish_that_cannot_get_through_is_kept_with_the_job_and_listed_with_it_until_it_is_sent` |
| a finish Wheelhouse refuses for good is not kept | wh-core `supervisor::tests::a_finish_wheelhouse_refuses_for_good_is_not_kept` |
| a verified chat message is run in its own pool, streamed, and finished with the answer and session | wh-core `supervisor::tests::a_verified_chat_message_is_run_in_its_own_pool_streamed_and_finished_with_the_answer_and_session` |
| chat messages are verified like offers, and an expired one is dropped | wh-core `supervisor::tests::chat_messages_are_verified_like_offers_and_an_expired_one_is_dropped` |
| the chat pool is two, apart from jobs: a third message fails cleanly, and jobs still run | wh-core `supervisor::tests::the_chat_pool_is_two_apart_from_jobs_a_third_message_fails_cleanly_and_jobs_still_run` |
| a signed chat.cancel stops the run, reported canceled | wh-core `supervisor::tests::a_signed_chat_cancel_stops_the_run_reported_canceled` |
| a malformed chat message fails with a reason; one with no id is dropped | wh-core `supervisor::tests::a_malformed_chat_message_fails_with_a_reason_one_with_no_id_is_dropped` |
| a chat run that fails on the usage limit pauses jobs and chats; an answer that merely mentions it does not | wh-core `supervisor::tests::a_chat_run_that_fails_on_the_usage_limit_pauses_jobs_and_chats_an_answer_that_merely_mentions_it_does_not` |
| a failing worktree fails the message with the reason | wh-core `supervisor::tests::a_failing_chat_worktree_fails_the_message_with_the_reason` |
| shutdown cancels running chats and waits for them to report | wh-core `supervisor::tests::shutdown_cancels_running_chats_and_waits_for_them_to_report` |
| a check that keeps failing goes back to the agent twice, then fails the run naming it; the finish lists every check | wh-core `supervisor::tests::a_check_that_keeps_failing_goes_back_to_the_agent_twice_then_fails_the_run_naming_it_the_finish_lists_every_check` |
| a fix that makes the checks pass lets the run succeed | wh-core `supervisor::tests::a_fix_that_makes_the_checks_pass_lets_the_run_succeed` |
| a check that cannot run is reported as such, is not retried, and fails the run; an optional one does not | wh-core `supervisor::tests::a_check_that_cannot_run_is_reported_as_such_is_not_retried_and_fails_the_run_an_optional_one_does_not` |

## daemon/test/workspace.test.mjs
| Node test | Rust counterpart |
|---|---|
| clone once, worktree on the job's branch, commit what was left, push, and remove | wh-core `workspace::tests::clone_once_worktree_on_the_jobs_branch_commit_what_was_left_push_and_remove` |
| a worktree can start from another branch; a missing one is a clear error | wh-core `workspace::tests::a_worktree_can_start_from_another_branch_and_a_missing_one_is_a_clear_error` |
| a continuation starts from the job's own pushed branch and counts only what it adds | wh-core `workspace::tests::a_continuation_starts_from_the_jobs_own_pushed_branch_and_counts_only_what_it_adds` |
| a continuation brings in what landed on its base; a conflict is left for the agent | wh-core `workspace::tests::a_continuation_brings_in_what_landed_on_its_base_and_a_conflict_is_left_for_the_agent` |
| a branch name that is not the job's shape is refused | wh-core `workspace::tests::a_branch_name_that_is_not_the_jobs_shape_is_refused` |
| continuing a stopped run whose kept worktree still holds the branch: its leftovers are saved and the branch is picked up | wh-core `workspace::tests::continuing_a_stopped_run_whose_worktree_still_holds_the_branch_picks_up_its_leftovers` |
| a chat worktree is detached, reused for the thread, and stays where the chat moved it | wh-core `workspace::tests::a_chat_worktree_is_detached_reused_for_the_thread_and_stays_where_the_chat_moved_it` |
| pruning removes chat worktrees unused for a day and keeps the recent ones | wh-core `workspace::tests::pruning_removes_chat_worktrees_unused_for_a_day_and_keeps_the_recent_ones` |

## cli/test/board.test.mjs
| Node test | Rust counterpart |
|---|---|
| outside a linked repo the board says how to set it up and exits 1 | - |
| login remembers the server, so board commands work in any folder; the team comes from the ref | - |
| issues lists with filters; it needs a team, from flags, the repo or the default | - |
| start claims the issue, moves it to In Progress and switches to its branch | - |
| comment and new send what was typed | - |
| a server error is printed and exits 1 | - |

## cli/test/config.test.mjs
| Node test | Rust counterpart |
|---|---|
| keys saved before 0.2.0 move from ~/.config/wheelhouse to ~/.wheelhouse on first use | - |
| wheelhouse.json and the saved key win over the Claude Code plugin's options | - |
| the plugin's options are the fallback, and WHEELHOUSE_* and flags come first | - |
| integrations lists Claude Code, marks it installed from Claude's plugin list, and runs its hook | - |
| doctor names the fix for each problem and exits non-zero when something is broken | - |
| read commands take --json for scripts | - |
| daemon forwards to the daemon next to the CLI, and passes its exit code through | - |
| plain wheelhouse shows a short overview, --help the full list, and an unknown command exits 1 | - |

## cli/test/login.test.mjs
| Node test | Rust counterpart |
|---|---|
| scripted login (--url and --key) saves the key with mode 0600 | - |
| tightens a credentials file that was left world-readable | - |
| login without a terminal explains how to script it and exits non-zero | - |
| init in a script with no key links the repo, does not prompt, and leaves the baseline for the plugin | - |
| init in a script still works with WHEELHOUSE_KEY | - |
| the login URL prompt defaults to the hosted Wheelhouse and init no longer requires --url | - |

## cli/test/outbox.test.mjs
| Node test | Rust counterpart |
|---|---|
| a git hook with no key only queues the landing and never calls the server | - |
| the session-start hook, which has the plugin's key, flushes the queue and deletes what it sent | - |
| a baseline left pending by a key-less init is sent by the next session start, quietly | - |
| a failing server leaves the queue intact, and the next flush sends it | - |
| a payload the server rejects (400) is dropped instead of blocking the queue forever | - |
| when the CLI also has a key, the git hook sends right away | - |
| nothing is queued for a branch other than the default | - |
| status shows what is queued | - |
| a repo that was never linked is left alone (no queue, no error) | - |

## cli/test/pre-mcp.test.mjs
| Node test | Rust counterpart |
|---|---|
| adds the session to the tool input and keeps every other field | - |
| overwrites a session the model passed, right or wrong | - |
| never sets a permission decision (must not auto-approve Wheelhouse tools) | - |
| fails open: no session id or no usable input means no output and exit 0 | - |
| the plugin's hooks.json wires it to the Wheelhouse MCP tools | - |
| an old wheelhouse.json with `project` is refused with a re-run-init message | - |

## cli/test/removal.test.mjs
| Node test | Rust counterpart |
|---|---|
| the briefing tells a reconnected, previously removed session about it and how to answer | - |
| the prompt hook doesn't repeat a removal it already told this session about | - |
| a new removal is reported once, then not again | - |
| with no removal the hook says nothing about it | - |

## cli/test/ui.test.mjs
| Node test | Rust counterpart |
|---|---|
| the output level follows the terminal | - |
| the header is one plain line for scripts and one short line on a narrow terminal | - |
