---
name: wheelhouse
description: How to work with Shipstack Wheelhouse (the team's kanban board and knowledge map, via the wheelhouse MCP tools). Use whenever the repo has a wheelhouse.json, when starting work on a ticket, when you've changed code that the map describes, or when the Stop hook asks you to reconcile the board and map.
---

# Working with Wheelhouse

Wheelhouse is the team's shared board and knowledge map. Humans and agents both read and write it. The project key is in `wheelhouse.json` at the repo root.

## Start of a session
- The plugin injects a briefing automatically (also in `.wheelhouse/context.md`). Use it instead of exploring the codebase from scratch.
- Call `get_changes` and handle what the team changed (moved/edited tickets, comments to you, stale nodes), then `ack_changes` with the highest seq handled.

## Working a ticket
1. `get_ticket_context` for the ticket, and read the files it lists first.
2. If it's agent-claimable, `claim_ticket`. Move it to "In Progress".
3. Ask questions and report progress with `comment`. Humans read these.
4. When done, move it to "Review" (never "Done" unless told to) with a comment summarising what changed.
5. Create follow-up tickets instead of silently expanding scope.

## Keeping the map true
- Changed files are tracked automatically. Before your turn ends, the Stop hook lists anything you owe: a ticket update, and map nodes whose files you changed.
- For each listed node: update its explainer with `upsert_node` (or `upsert_table` for tables), or call `mark_fresh` if it's still accurate. While your changes haven't landed on the default branch, these are saved as **drafts** and publish automatically when the code lands. That is expected.
- Add nodes, `implements` links and flows for new parts that matter. Record non-obvious choices with `upsert_decision`.

## Etiquette
- Pass `expected_version` on ticket edits. On a conflict, a human changed it: re-read, don't retry blindly.
- Write explanations for a teammate who hasn't read the code: one-line summary, purpose, responsibilities, interfaces, gotchas.
