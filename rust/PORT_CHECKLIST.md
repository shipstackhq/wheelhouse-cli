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
| a request that is never answered is given up on instead of hanging | - |
| a stream that goes quiet is dropped with a clear error, so the daemon reconnects | - |
| a stream that keeps receiving keep-alives stays open, and the caller can still end it | - |

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
| a run with a key reads Wheelhouse through a private MCP config file that is gone afterwards | partial: `wh-core `tests/runner_golden.rs::claude_arguments_match_node` (`mcp`) covers the flags; the 0600 temp file is not ported yet |
| a delegated run uses its agent's definition, and is told to check done and ask | wh-core `tests/runner_golden.rs::claude_arguments_match_node` (`effortAndAgent`) and `prompts_match_node` (`agent`) |
| an answered question is handed to the continuing run | wh-core `tests/runner_golden.rs::prompts_match_node` (`continuationAnswered`) |
| the offer's effort is passed to Claude, and left out when there is none | wh-core `tests/runner_golden.rs::claude_arguments_match_node` (`effortAndAgent`, `plain`) |
| a review reads the pull request, may not edit, and is told to give a verdict | wh-core `tests/runner_golden.rs::prompts_match_node` (`review`) and `claude_arguments_match_node` (`review`) |
| a delegated run is asked for learnings; LEARNING lines leave the summary and become a list | wh-core `tests/runner_golden.rs::stream_lines_match_node` (learnings) and `prompts_match_node` (`agent`) |
| stream-json becomes short progress events | wh-core `tests/runner_golden.rs::stream_lines_match_node` |
| runs the CLI in the worktree and reports its result | - |
| an abort stops the agent and its children | - |
| a missing CLI is a clear failure | - |
| a chat run is read-only: file tools and read-only git, no MCP, no hooks, partial messages, the prompt as typed | - |
| the chat parser: text deltas, tool calls, and the result; the full text only when no deltas came | - |
| runs a chat message and reports deltas, tools and the whole answer; an abort stops it | - |
| chat with a key: MCP config in a 0600 file (not argv), strict config kept, mcp__wheelhouse allowed, rules switch; without one, none of it | - |
| chat rules offer Wheelhouse writes only when mcp.write is true | - |
| write tool summaries: create_issue shows the title, comment the issue | - |
| the MCP tool summary is short: 'wheelhouse: get_issue WH-42' | wh-core `tests/runner_golden.rs::tool_summaries_match_node` (`mcp`) |
| a keyed chat run writes the config 0600 outside cwd, keeps the token out of argv and events, and deletes it after | - |
| scrub: paths read relative to the run's checkout, the rest of home as ~ | wh-core `tests/runner_golden.rs::scrub_matches_node` |

## daemon/test/supervisor.test.mjs
| Node test | Rust counterpart |
|---|---|
| a verified offer is accepted, run, and finished with what the run reported | - |
| a run's learnings go with its finish | - |
| a review commits nothing and pushes nothing, and still finishes with what it said | - |
| unverifiable messages are dropped without a word to the server | - |
| never more than the pool size at once; the next offer is refused for want of a slot | - |
| a settings message changes the pool size | - |
| a signed cancel stops the run; it is reported canceled, and the work still pushed | - |
| a failing step fails the job with the reason, and frees the slot | - |
| a run that hits the usage limit pauses new offers | - |
| a Stop from Wheelhouse says it was requested; the daemon going down says that instead | - |
| shutdown cancels what is running and waits for it to report | - |
| a run whose branch a newer run took over stops without committing or pushing, and keeps its worktree | - |
| a finish that cannot get through is kept with the job and listed with it until it is sent | - |
| a finish Wheelhouse refuses for good is not kept | - |
| a verified chat message is run in its own pool, streamed, and finished with the answer and session | - |
| chat messages are verified like offers, and an expired one is dropped | - |
| the chat pool is two, apart from jobs: a third message fails cleanly, and jobs still run | - |
| a signed chat.cancel stops the run, reported canceled | - |
| a malformed chat message fails with a reason; one with no id is dropped | - |
| a chat run that fails on the usage limit pauses jobs and chats; an answer that merely mentions it does not | - |
| a failing worktree fails the message with the reason | - |
| shutdown cancels running chats and waits for them to report | - |
| a check that keeps failing goes back to the agent twice, then fails the run naming it; the finish lists every check | - |
| a fix that makes the checks pass lets the run succeed | - |
| a check that cannot run is reported as such, is not retried, and fails the run; an optional one does not | - |

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
