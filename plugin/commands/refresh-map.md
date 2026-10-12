---
description: Re-describe Wheelhouse map nodes whose code changed (stale nodes)
argument-hint: "[PROJECT_KEY]"
---

Refresh stale parts of the Wheelhouse knowledge map for project `$ARGUMENTS` (or `project` from `wheelhouse.json`).

1. Call `get_stale_nodes`.
2. For each one: `get_node`, re-read its anchored files and the files in its stale reason, then either update the explainer fields that are now wrong, or call `mark_fresh` if it's still accurate. Pass the current `git rev-parse HEAD` as `described_sha` / `sha`.
3. Add nodes, edges and `implements` links for anything new that matters; update flows whose steps changed.
4. Record an ADR for any non-obvious decision embodied in the change.
Report what you updated.
