---
description: Build the Wheelhouse knowledge map for this repo (concepts → systems → data → code, flows, decisions, glossary)
argument-hint: "[PROJECT_KEY] [focus]"
---

Build the Wheelhouse knowledge map for this repository. Project key: first word of `$ARGUMENTS`, otherwise `project` in `wheelhouse.json`. Anything after the key is an optional focus area.

Goal: a teammate should understand this codebase at a high level from the map alone, without reading code.

1. Call `get_graph` to see what already exists; extend and correct it, don't duplicate.
2. Explore the repo (README, entry points, routing, schema, package boundaries), then write the map top-down:
   - Concept layer (`layer: "concept"`): 4–10 product concepts, capabilities and actors in product language. Define project vocabulary with `upsert_term`.
   - System layer: deployable apps, datastores, queues, external services as top-level nodes, major modules nested with `parent`, `depends` edges labelled with the protocol, `implements` links to concepts.
   - Data layer: every table/view/enum with `upsert_table` (full columns, `references: "table.column"` for foreign keys, indexes, anchors, implements).
   - Code layer: only the directories, files and endpoints that matter, each anchored and linked with `implements`.
3. For every system/data/code node set `anchors` and the explainer (summary, purpose, responsibilities, interfaces, gotchas). Pass `described_sha` = `git rev-parse HEAD`.
4. Write 3–6 flows (`upsert_flow`) for the most important paths.
5. Record ADRs (`upsert_decision`) for non-obvious choices you find evidence of.
6. Summarise what you mapped and what you were unsure about.
