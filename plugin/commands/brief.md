---
description: Load the Wheelhouse briefing (what the project is, how it's built, work in flight, what changed)
argument-hint: "[PROJECT_KEY]"
---

Load the current Wheelhouse context. Use project key `$ARGUMENTS` if given, otherwise the `project` field in `wheelhouse.json` at the repository root.

1. Call `get_briefing` for the project and read it carefully: it replaces exploring the codebase from scratch.
2. Call `get_changes`; handle anything addressed to you, then `ack_changes` with the highest seq you handled.
3. If you are about to work a ticket, call `get_ticket_context` for it and read the files it lists first.
4. Summarise in 3–5 lines what you now know and what you'll do next.
