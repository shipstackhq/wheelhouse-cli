# @shipstack/wheelhouse-cli

```
npm install -g @shipstack/wheelhouse-cli
```

The command-line companion for Shipstack Wheelhouse. It links a repository to a Wheelhouse project, reports what lands on the default branch, and runs the Claude Code hooks that keep the board and knowledge map in sync.

It has no dependencies and runs on Node 18+ with git.

## Commands

```
wheelhouse init --project KEY [--url URL] [--key wh_...] link this repo, install git hooks, record baseline hashes
wheelhouse login [--url URL]                             prompt (URL defaults to https://wheelhouse.shipstack.run) for an API key (hidden), check it, store it (scripts: --url URL --key wh_...) (~/.config/wheelhouse/credentials.json, mode 600)
wheelhouse status                                        config and server state
wheelhouse context [TICKET]                              print the project briefing (and write .wheelhouse/context.md) or a ticket's context pack
wheelhouse sync                                          report working-copy changes as a session (for workflows without Claude Code)
wheelhouse land [--baseline|--flush] [--from REF --to REF]  report what landed on the default branch. Git hooks only queue it locally; --flush sends the queue
wheelhouse hook <event>                                  Claude Code hook runner (used by the plugin)
```

## Configuration

`wheelhouse.json` at the repo root holds `url`, `project` and `defaultBranch`. Commit it.

The API key is resolved in this order:

1. `--key`
2. `WHEELHOUSE_KEY`
3. the Claude Code plugin's `api_key` option
4. `~/.config/wheelhouse/credentials.json`

`WHEELHOUSE_URL` and `WHEELHOUSE_PROJECT` override the file.

## How sync works

- **Content hashes** are git blob ids, so the server can compare working copies and commits without ever seeing file contents.
- **Landing.** `post-commit`, `post-merge` and `post-rewrite` git hooks queue what landed on the default branch in `.wheelhouse/outbox` and never need a key. Claude Code session starts and turn ends (which have the plugin's key) send the queue, and if this machine also has a CLI key the git hook sends right away.
  - Pending map drafts for the affected nodes are published.
  - Nodes whose files changed without a draft are flagged stale.
  - Reverting a file clears the flag.
- **Sessions.** The hook runner records which files a Claude Code session changed, then checks with the server what the session owes before a turn ends.

Hooks always fail open, and errors go to `.wheelhouse/hook.log`.
