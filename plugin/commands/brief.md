---
description: Load the Wheelhouse briefing (what the team is, how it's built, work in flight, what changed)
argument-hint: "[TEAM_KEY]"
---

Load the current Wheelhouse context. Use team key `$ARGUMENTS` if given, otherwise the `team` field in `wheelhouse.json` at the repository root.

1. Call `get_briefing` with `team` and read it carefully: it replaces exploring the codebase from scratch.
2. Call `get_changes`; handle anything addressed to you, then pass the highest seq you handled as `ack` on your next call.
3. If you are about to work an issue, call `get_issue` for it and read the files it lists first.
4. Summarise in 3–5 lines what you now know and what you'll do next.
