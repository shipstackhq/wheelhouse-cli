# Shipstack Wheelhouse for Claude Code

This plugin keeps your Wheelhouse board and knowledge map in sync with what Claude actually changes. It does that deterministically through hooks, without relying on Claude remembering to.

## What it does

- **Session start** (including after `/clear` and compaction): reports any commits that landed on the default branch, then injects a compact project briefing and writes it to `.wheelhouse/context.md`.
- **Every edit** (Edit / Write / MultiEdit / NotebookEdit), plus files changed by Bash commands: records the file and its content hash against the session. Runs in the background.
- **Every prompt**: notices files changed outside Claude (e.g. by you in your editor), and mentions new changes from the team.
- **Before Claude finishes a turn**: if the session changed files, asks Wheelhouse what it still owes. That means a ticket that reflects the work, plus updates or confirmations for the map nodes whose files changed. If anything is owed, Claude is told exactly what and keeps going. There are at most two reminders per prompt, so it never traps the session.
- Map updates written against code that hasn't landed on the default branch are saved as **drafts**. They publish automatically when the code lands.

It also adds the Wheelhouse MCP server (board, map, briefing tools), a `wheelhouse` skill, and these commands:

- `/wheelhouse:setup KEY` links this repo to a project
- `/wheelhouse:brief` loads the briefing and the queue
- `/wheelhouse:map-codebase` and `/wheelhouse:map-schema` build the knowledge map
- `/wheelhouse:refresh-map` re-describes stale nodes
- `/wheelhouse:status` shows the link and sync state

## Install

```
/plugin marketplace add shipstackhq/wheelhouse-cli
/plugin install wheelhouse@shipstack
```

You can also add the marketplace from a local clone: `/plugin marketplace add /path/to/wheelhouse-cli`.

When enabled, Claude Code asks for:

- **Wheelhouse URL**, defaults to `https://wheelhouse.shipstack.run`; change it only for your own server (e.g. `http://localhost:3456` for local development)
- **API key** from Wheelhouse → Agents & API keys. It's stored in your OS credential store, never in a file.

Then, in each repository:

```
/wheelhouse:setup WH
```

This writes `wheelhouse.json` (commit it), adds `.wheelhouse/` to `.gitignore`, installs git hooks, and records baseline content hashes.

You don't need a second key. Git hooks run outside Claude Code and can't read the plugin's stored key, so they only queue what landed (in `.wheelhouse/outbox`); the plugin sends the queue, and the one-time baseline, at the next session start and turn end. If you also commit outside Claude sessions and want those reported immediately, run `npx @shipstack/wheelhouse-cli login` once (it asks for the URL, press Enter for `https://wheelhouse.shipstack.run`, and your key, typed hidden). For scripts, `login --url <url> --key wh_...` still works.

Commits are also picked up at the next Claude session start or turn end on the default branch, so this is a backstop, not a requirement.

## Requirements and behaviour

- Node 18+ and git. Hooks do nothing in repos without a `wheelhouse.json`.
- Every hook fails open: if Wheelhouse is unreachable, Claude's work continues and the error is logged to `.wheelhouse/hook.log`.
- Only file paths and git blob hashes are sent, never file contents. Ticket comments and map explanations are written by Claude through the MCP tools.

`scripts/wheelhouse.mjs` is a copy of the CLI in `../cli/`. Update it with `npm run sync-plugin` at the repo root.
