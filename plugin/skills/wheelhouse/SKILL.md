---
name: wheelhouse
description: How to work with Shipstack Wheelhouse (the team's board and knowledge map, via the wheelhouse MCP tools). Use whenever the repo has a wheelhouse.json, when starting work on an issue, when you've changed code that the map describes, or when the Stop hook asks you to reconcile the board and map.
---

# Working with Wheelhouse

Wheelhouse is the team's shared board and knowledge map. Humans and agents both read and write it. Every tool takes `team`: the key in `wheelhouse.json` at the repo root. A project is Linear's kind: work that spans teams and cycles.

## Start of a session
- The plugin injects a briefing automatically (also in `.wheelhouse/context.md`). Use it instead of exploring the codebase from scratch.
- Call `get_changes` and handle what the team changed (new/edited/moved issues, comments to you, stale nodes and docs), then pass the highest seq handled as `ack` on your next call.

## Working an issue
1. `list_issues` to find work (filter assignee "me", cycle "current"); `get_issue` for the one you take, and read the files it lists first.
2. `update_issue` with status "in_progress" and assignee "me" (a claim, for agent-claimable issues).
3. Ask questions and report progress with `comment`. Humans read these.
4. When done, `update_issue` to status "review" (never "done" unless told to) with a comment summarising what changed.
5. `create_issue` for follow-up work instead of silently expanding scope.

## Keeping the map true
- Changed files are tracked automatically. Before your turn ends, the Stop hook lists anything you owe: an issue update, and map nodes whose files you changed.
- For each listed node: update its explainer with `update_map` (or `write_doc` for docs), or call `mark_fresh` if it's still accurate. While your changes haven't landed on the default branch, these are saved as **drafts** and publish automatically when the code lands. That is expected.
- Add nodes, `implements` links and flows for new parts that matter. Record non-obvious choices with `record_decision`.

## Etiquette
- Pass `expected_version` on edits. On a conflict, a human changed it: re-read, don't retry blindly.
- Write explanations for a teammate who hasn't read the code: one-line summary, purpose, responsibilities, interfaces, gotchas.
