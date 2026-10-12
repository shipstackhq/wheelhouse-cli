# Shipstack Wheelhouse: guide for AI agents

You are reading this because you are an AI agent (for example Claude Code) that has been connected to Wheelhouse, or is about to be. This file explains what Wheelhouse is, what you can do with it today, what it is planned to do later, and exactly how to use it well.


## What Wheelhouse is

Wheelhouse is the software development lifecycle manager in the Shipstack product family. It is a shared workspace for a small team of humans and their AI agents. Humans use it through a web app. Agents use it through an MCP server and an equivalent REST API. Both sides read and write the same data.

It has two halves.

The board is a kanban board of tickets. Humans plan, prioritise and review work there. Agents pick up work, report progress, ask questions and create follow-up tickets there.

The knowledge map is a living explanation of the codebase, written mostly by agents and read mostly by humans. Its purpose is to let a team that moves fast with AI coding understand its own system at a high level without reading the code.

Everything either side changes is written to an append-only event log. Each agent has its own read position (a cursor) in that log, so you can always ask "what did the team change since I last looked?" and get exactly that. This queue is how humans talk to you between sessions: they move tickets, change priorities, comment, and edit the map, and you pick those changes up the next time you check.


## Core ideas you need to know

Team and projects. A team owns projects. Each project has a short uppercase key such as WH or AT. Every tool takes a project key (or id).

Tickets. A ticket lives in a column (by default Backlog, Todo, In Progress, Review, Done). Tickets are referred to as KEY-NUMBER, for example WH-12. A ticket has a markdown body, priority (low, medium, high, urgent), labels, an optional assignee, a version number, comments, and links to map nodes.

Agent-claimable tickets. Humans mark some tickets as "AI-ready" (agentClaimable). Only those are meant for you to pick up on your own. Claim one before working on it so other agents skip it.

Versions and conflicts. Tickets carry a version. When you edit a ticket, pass expected_version with the version you last read. If a human changed it in the meantime, the call fails with a conflict instead of overwriting their work. When that happens, re-read the ticket, reconsider, and retry.

Your identity. You act through an API key. Everything you do is attributed to that key's name and shown to humans with a robot marker. Your own changes never appear in your own queue.

The knowledge map has four linked layers.
The concept layer describes what the product does in plain language: concepts, capabilities and actors. No file names.
The system layer describes what runs where: services, modules, components, databases, queues and external services, with modules nested inside their service.
The data layer describes what is stored and how it relates: tables, views and enums with their columns, foreign keys and cardinality, drawn as an entity-relationship diagram.
The code layer describes where things live: directories, files, symbols and endpoints.

Edges on the map come in three kinds.
A depends edge means "uses or calls", usually within one layer, for example web depends on api with the label REST.
An implements edge links a lower layer to the higher-level thing it realizes. The direction is always code to data to system to concept. For example the file src/lib/service.ts implements the system node Service layer, which implements the concept AI change queue.
A references edge is a foreign key between two tables. You normally do not create these directly. You set a column's references field to "table.column" and the edge is derived from it automatically.

Node explainers. Every node has a structured explanation: summary (one line), purpose (why it exists), responsibilities (what it owns), interfaces (inputs, outputs, APIs, events), gotchas (invariants and traps), and free-form notes. Write these for a teammate who has not read the code.

Anchors and freshness. System, data and code nodes carry anchors: repository paths or globs that the node describes, for example src/lib/service.ts or src/app/mcp/** or src/db/schema.ts#users. When files change and are reported, the most specifically anchored node on each layer is marked stale. Stale nodes show a warning to humans and appear in your queue until someone re-describes them or confirms they are still accurate.

Flows. A flow is a named, ordered walk through the map that explains how something works end to end, for example "User signs up" or "Agent picks up a ticket". Each step points at a node and has a short title and an optional detail. Humans can play flows step by step on the canvas. Flows are the single most effective way to explain a system.

Decisions. Lightweight architecture decision records (ADRs), numbered ADR-1, ADR-2 and so on. Each has context, the decision and its consequences, and is attached to the nodes it shapes. They record why something is the way it is, so nobody "fixes" something that was deliberate.

Glossary. Project vocabulary with definitions. Terms are underlined with a hover definition wherever they appear in explanations, so define words a newcomer would not know.

Node references. Wherever a tool asks for a node you may pass its id, its exact name, or "layer:name" (for example system:Postgres or data:users). Use the layer prefix when the same name exists on more than one layer; the tool will tell you if a name is ambiguous.


## How to connect

Get an API key from a human. They create one in the web app under Agents and API keys. Keys look like wh_ followed by random characters. Never commit a key to a repository.

For Claude Code, run this in the repository you work in:

claude mcp add --transport http wheelhouse https://YOUR-WHEELHOUSE-HOST/mcp --header "Authorization: Bearer wh_YOUR_KEY"

For local development the host is usually http://localhost:3456 or http://localhost:3000.

The MCP server is stateless and speaks JSON-RPC over HTTP (Streamable HTTP with JSON responses). It provides tools and prompts. The same operations are available over REST: send POST /api/v1/TOOL_NAME with the tool's arguments as a JSON body and the same Authorization header. GET /api/v1 lists every tool with its input schema.


## Starting a session

Call get_briefing first. It returns a compact, budgeted overview: what the project is, how it is built, the work in flight (yours first), what changed since you last looked, decisions, gotchas, stale and in-flux areas, flows and the glossary. Read it instead of exploring the codebase from scratch. With the Claude Code plugin installed, the briefing is injected automatically at session start and saved to .wheelhouse/context.md.

Before working a ticket, call get_ticket_context. It returns the ticket and its discussion, the parts of the system it touches with their explanations, what they connect to on other layers, the tables involved, flows passing through, applicable decisions, and the files to read first.


## The working loop

Follow this loop on every task.

1. Check your queue first. Call get_changes for the project. Read every change: new or edited tickets, moved tickets, priority changes, comments (especially ones addressed to you), and map edits. The response also includes staleNodes, the map nodes whose code changed since they were described. Act on what matters, then call ack_changes with the highest seq you handled. Do not ack changes you have not read.

2. Pick up work. Use get_board to see the columns and tickets. Prefer tickets that are agent-claimable and unclaimed, in priority order, unless a human asked you for something specific. Call get_ticket to read the full description, comments, linked map nodes and history. If the ticket is agent-claimable, call claim_ticket. Move it to In Progress with move_ticket.

3. Read the map before structural work. Call get_graph (optionally for one layer), get_node for the parts you will touch, and get_schema before changing the database. Use the map to understand intent and constraints, including decisions and gotchas.

4. Talk to the team through comments. Use comment on the ticket for questions, decisions you made, and a short summary of what you changed when you finish. Humans read these. If you are blocked on a decision that belongs to a human, ask in a comment, leave the ticket where it is, and move on to other work.

5. Finish properly. Move the ticket to Review when your work is done. Never move a ticket to Done yourself unless a human explicitly told you to; Done means a human accepted it. Create new tickets with create_ticket for follow-up work or problems you discovered instead of silently expanding the scope of the current ticket.

6. Keep the map true. After finishing a change:
Call report_changes with the files you touched (and the commit sha if you have one). It tells you which nodes became stale.
For each stale node, re-read its anchored files and either update the explainer fields that are now wrong, or call mark_fresh if the explanation is still accurate. Pass described_sha (the output of git rev-parse HEAD) when you have just re-read the code.
If you added a meaningful new service, module, file, endpoint or table, add nodes for it with anchors and explainers, and add implements links to the higher-level nodes it serves.
If you changed the schema, update the affected tables with upsert_table.
If a flow's steps changed, update the flow with upsert_flow.
If you made a choice a teammate might later question or undo, record it with upsert_decision.
Link the ticket to the nodes it touched with link_ticket_node.

7. Respect humans' edits. If a call fails with a conflict, someone changed the thing you were editing. Re-read it and decide again. Never overwrite a human's change by retrying blindly.


## Drafts and automatic sync (Claude Code plugin)

When the Wheelhouse plugin is installed, hooks keep the board and map in sync deterministically:
Every file you edit, and every file a Bash command changes, is recorded against your session automatically.
Before your turn ends, a Stop hook asks Wheelhouse what the session still owes: a ticket that reflects the work (claimed or created, moved to In Progress, and updated since you started changing files), and an update or confirmation for each map node whose files you changed. If anything is owed you are told exactly what, and you should do it before finishing.
While your changes have not landed on the default branch, your map updates (upsert_node, upsert_table, mark_fresh) are saved as drafts instead of changing the shared map. The response says "draft": true. That is expected. Drafts publish automatically when the code lands, and are discarded if the work is abandoned. list_drafts shows pending drafts.
Files changed outside your session (for example by a human in their editor) are tracked as in flux but are not your debt.
Without the plugin, follow the same loop by hand: report_changes with the files you touched, then update or confirm the affected nodes.


## Building the knowledge map from scratch

The server ships prompts that appear as slash commands in Claude Code. Prompts are not tools, so they do not appear in the tool list.

/mcp__wheelhouse__map_codebase KEY builds the whole map top-down: concepts, then systems, then the data layer, then the code layer, then key flows, decisions and glossary terms.

/mcp__wheelhouse__map_schema KEY maps the database schema into the data layer from the repository's schema or migration files.

/mcp__wheelhouse__refresh_map KEY re-describes every stale node.

/mcp__wheelhouse__explain KEY TOPIC explains part of the system from the map, and fills gaps in the map where you had to read code to answer.

If prompts are unavailable in your client, do the same work directly with the tools. A good order is:
First, call get_graph to see what exists, so you extend it rather than duplicate it.
Second, write 4 to 10 concepts in product language and define project vocabulary with upsert_term.
Third, write the system layer: deployable apps, datastores, queues and external services as top-level nodes, their major modules nested with parent, depends edges with labels for protocols, and implements links to concepts.
Fourth, write the data layer with upsert_table for every table, view and enum, including all columns and foreign keys.
Fifth, write the code layer: only the directories, files and endpoints that matter for understanding, each anchored and linked with implements to what it realizes. Aim for signal, not a file listing.
Sixth, write 3 to 6 flows for the most important paths.
Seventh, record decisions you find evidence of in comments, docs or commit history.


## Quality bar for what you write

Write for a teammate who has not read the code and is in a hurry.
Summaries are one line.
Purpose says why the thing exists, not what its code does line by line.
Responsibilities is a short bullet list of what it owns.
Interfaces names the inputs, outputs, endpoints, events and tables it talks to.
Gotchas are the invariants and traps that would bite someone changing it.
Prefer fewer, well-explained nodes over many thin ones.
Use markdown in bodies. Refer to other nodes by name.
Keep ticket bodies actionable: what and why, plus acceptance criteria.
Do not invent behaviour. If you are unsure, say so in the node notes or a ticket comment.


## Tool reference

Board and tickets:
list_projects lists the team's projects.
create_project creates a project with the default columns.
get_board returns the columns in order with their tickets, versions and flags.
get_ticket returns one ticket in full: body, comments, linked nodes and history.
create_ticket creates a ticket (optionally in a column, with priority, labels and linked nodes).
update_ticket edits title, body, priority or labels; pass expected_version.
move_ticket moves a ticket to a column by name, optionally at a position; pass expected_version.
claim_ticket claims an agent-claimable ticket for you.
comment posts a markdown comment on a ticket.

Queue and activity:
get_changes returns changes since your cursor, excluding your own, plus stale map nodes.
ack_changes advances your cursor to a seq.
get_activity returns the recent activity feed for a project.

Knowledge map:
get_graph returns nodes, edges, flows, decisions and glossary terms; pass layer to filter.
get_node returns one node in full with its neighbours on every layer, flows, decisions and tickets.
upsert_node creates or updates a node by id or by name within a layer.
delete_node deletes a node and everything nested inside it.
upsert_edge creates a depends or implements edge (or a manual references edge between tables).
unlink_nodes removes edges between two nodes.
link_ticket_node links or unlinks a ticket and a node.

Data layer:
get_schema returns every table, view and enum with columns, indexes and relationships with cardinality.
upsert_table creates or updates a table, view or enum with its full column list. Columns replace the previous list, so always send every column. Set a column's references to "table.column" for foreign keys. Targets that do not exist yet are returned as unresolved and link up automatically once those tables exist, so order does not matter. For enums, list each value as a column with just a name.

Flows, decisions and glossary:
get_flows, upsert_flow and delete_flow manage flows. upsert_flow replaces all steps.
list_decisions and upsert_decision manage ADRs. Omit number to create, pass number to update.
list_terms, upsert_term and delete_term manage the glossary.

Context:
get_briefing returns the session briefing as markdown.
get_ticket_context returns everything needed to work one ticket, including the files to read first.
list_drafts lists map updates waiting for their code to land.

Freshness:
report_changes reports changed files and returns the nodes it marked stale and any files no node anchors.
get_stale_nodes lists stale nodes with reasons and anchors.
mark_fresh clears a node's stale flag when its explanation is still accurate.


## Example calls

Check the queue:
get_changes with {"project": "WH"}

Acknowledge what you handled:
ack_changes with {"project": "WH", "seq": 214}

Start a ticket safely:
get_ticket with {"ticket": "WH-12"}
claim_ticket with {"ticket": "WH-12"}
move_ticket with {"ticket": "WH-12", "column": "In Progress", "expected_version": 3}

Describe a system node:
upsert_node with {"project": "WH", "name": "Service layer", "layer": "system", "summary": "Single write path for all mutations", "purpose": "Keeps every change team-scoped, versioned and logged.", "anchors": ["src/lib/service.ts"], "implements": ["AI change queue"], "described_sha": "abc1234"}

Map a table:
upsert_table with {"project": "WH", "name": "tickets", "summary": "A unit of work on the board", "columns": [{"name": "id", "type": "uuid", "pk": true}, {"name": "project_id", "type": "uuid", "references": "projects.id", "onDelete": "cascade"}, {"name": "title", "type": "text"}], "anchors": ["src/db/schema.ts#tickets"], "implements": ["Shared board", "system:Postgres"]}

Report your changes after finishing:
report_changes with {"project": "WH", "files": ["src/lib/service.ts", "src/db/schema.ts"], "sha": "def5678", "summary": "Added ticket archiving"}


## Errors

Errors come back as a tool result marked as an error, with a code and a message.
not_found means the project, ticket, node, flow or term does not exist.
invalid means the input was wrong; the message says what to fix, for example an unknown column name (the valid names are listed), an ambiguous node name, a kind that does not belong on that layer, or an implements edge pointing the wrong way.
conflict means someone changed the ticket since you read it; re-read and retry.
forbidden means the action is not allowed, for example claiming a ticket that is not marked agent-claimable.
An HTTP 401 means the API key is missing, wrong or revoked.


## What humans see

Humans see your work live. Tickets and comments you create carry a robot marker. The project header shows each agent's queue as "in sync" or "N queued". A dashboard shows progress across projects, throughput, how much work agents versus humans are doing, each agent's queue, and what needs a human's attention: tickets waiting for review, urgent tickets and stale explanations. On the map, humans can switch layers, select a concept and see the systems, tables and code behind it, play flows, and ask Claude questions answered from the map. This is why keeping the map accurate matters: it is how the team understands what you built.


## What Wheelhouse can be used for today

Coordinating work between a small team and several AI agents on one board.
Handing work to agents asynchronously and reviewing it in a Review column.
Asking an agent questions and getting answers in ticket comments.
Giving a team a high-level, always-current explanation of a codebase across concepts, systems, data and code.
Onboarding new teammates or new agents through flows, explainers, decisions and the glossary.
Seeing which explanations went stale after code changes, from agent reports or a post-commit hook.
Tracking progress and agent involvement across projects on the dashboard.
Answering questions about the system from the map with the Ask feature (when the server has a Claude API key).


## What Wheelhouse is planned to do (not available yet)

Do not rely on these until they appear in the tool list or the server instructions.
Real-time push updates instead of polling, so humans and agents see changes instantly.
Webhooks and integrations, for example with GitHub pull requests and CI, so tickets and the map update from repository events.
A command-line tool that extracts the real import and dependency graph from code, so code-layer edges come from facts while agents write the explanations.
More dashboard widgets and metrics, such as cycle time, review latency and per-agent quality signals.
Fuller team management: invite emails, password reset, multiple teams per person, and sign-in through Shipstack Auth.
Planning features such as sprints, estimates and milestones.
Richer agent coordination, such as routing tickets to specific agents by skill or tag.


## Rules of thumb

Check get_changes at the start of every task and ack only what you handled.
Work from tickets; ask in comments; never move tickets to Done on your own.
Pass expected_version on ticket edits and treat conflicts as a signal to re-read.
Read the map before structural changes and update it after them.
Report the files you change, then clear the stale nodes you caused.
Record non-obvious decisions as ADRs.
Write explanations for humans who have not read the code.
