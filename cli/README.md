# @shipstack/wheelhouse-cli

```
npm install -g @shipstack/wheelhouse-cli
```

The command-line companion for Shipstack Wheelhouse. It links a repository to a Wheelhouse team, reports what lands on the default branch, and runs the Claude Code hooks that keep the board and knowledge map in sync.

It has no dependencies and runs on Node 18+ with git.

## Commands

```
wheelhouse init --team KEY [--url URL] [--key wh_...] link this repo, install git hooks, record baseline hashes
wheelhouse login [--url URL]                             prompt (URL defaults to https://wheelhouse.shipstack.run) for an API key (hidden), check it, store it (scripts: --url URL --key wh_...)
wheelhouse status                                        config and server state
wheelhouse issues [--mine] [--status S] [--cycle current] [--text T]  open issues, most urgent first (--json)
wheelhouse issue WH-12                                   an issue with its description and comments (--json)
wheelhouse start WH-12 [--no-branch]                     claim it, move it to In Progress, switch to branch wh/WH-12-<title>
wheelhouse done WH-12                                    move it to In Review
wheelhouse comment WH-12 "text"                          comment on it
wheelhouse new "title" [--priority P] [--cycle current]  create an issue
wheelhouse daemon status | pair --server URL | unpair | run | install [--dry-run] | uninstall | logs [-n N] [-f]
                                                         the agent daemon on this machine (forwards to the daemon in this checkout)
wheelhouse doctor                                        check the server, key, repo link, git hooks, daemon and integrations; a fix for each problem
wheelhouse whoami                                        the server, team and key in use, where each came from, and whether the key works
wheelhouse context [ISSUE]                               print the team briefing (and write .wheelhouse/context.md) or an issue's context
wheelhouse sync                                          report working-copy changes as a session (for workflows without Claude Code)
wheelhouse land [--baseline|--flush] [--from REF --to REF]  report what landed on the default branch. Git hooks only queue it locally; --flush sends the queue
wheelhouse integrations [NAME COMMAND]                   list agent integrations (✓ installed), or run one: `integrations claude-code hook <event>` is what the plugin calls; `integrations claude-code install` sets the plugin up
```

## Configuration

`wheelhouse.json` at the repo root holds `url`, `team` and `defaultBranch`. It is personal and gitignored; each teammate runs setup once.

Each setting comes from the first of these that is set (`wheelhouse whoami` shows which one won):

1. a flag: `--url`, `--key`, `--team`
2. `WHEELHOUSE_URL`, `WHEELHOUSE_KEY`, `WHEELHOUSE_TEAM`
3. the key saved by `wheelhouse login`, per server URL: the macOS Keychain, or `~/.wheelhouse/credentials.json` (mode 600) elsewhere
4. `wheelhouse.json` (url, team)
5. `~/.wheelhouse/config.json`: the default server (saved by `wheelhouse login`) and team (saved by your first `wheelhouse init`), so board commands work in any folder. An issue ref like `WH-12` also names its team.
6. the Claude Code plugin's `server_url` and `api_key` options, as a fallback

Keys saved by older versions in `~/.config/wheelhouse/credentials.json` move over on first use.

## How sync works

- **Content hashes** are git blob ids, so the server can compare working copies and commits without ever seeing file contents.
- **Landing.** `post-commit`, `post-merge` and `post-rewrite` git hooks queue what landed on the default branch in `.wheelhouse/outbox` and never need a key. Claude Code session starts and turn ends (which have the plugin's key) send the queue, and if this machine also has a CLI key the git hook sends right away.
  - Pending map drafts for the affected nodes are published.
  - Nodes whose files changed without a draft are flagged stale.
  - Reverting a file clears the flag.
- **Sessions.** The hook runner records which files a Claude Code session changed, then checks with the server what the session owes before a turn ends.

Hooks always fail open, and errors go to `.wheelhouse/hook.log`.
