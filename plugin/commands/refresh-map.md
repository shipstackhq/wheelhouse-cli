---
description: Re-describe Wheelhouse map nodes whose code changed (stale nodes)
argument-hint: "[TEAM_KEY]"
---

Refresh stale parts of the Wheelhouse knowledge map for team `$ARGUMENTS` (or `team` from `wheelhouse.json`).

1. Call `get_stale` (stale nodes and docs).
2. For each one: `get_map` (`node`), re-read its anchored files and the files in its stale reason, then either update the explainer fields that are now wrong (`update_map`; `write_doc` for docs), or call `mark_fresh` if it's still accurate. Pass the current `git rev-parse HEAD` as `described_sha` / `sha`.
3. Add nodes, edges and `implements` links for anything new that matters; update flows whose steps changed.
4. Record a decision (`record_decision`) for any non-obvious decision embodied in the change.
Report what you updated.
