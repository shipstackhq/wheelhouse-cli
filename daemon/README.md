# Wheelhouse agent daemon

> **Status: in development. Nothing here runs yet, and the package is marked `private` so it cannot be published by accident.**

The daemon is a background service for a machine you own. It lets Wheelhouse hand a ready ticket to an agent on that machine, and it supervises the work and reports back. It is the counterpart of a self-hosted CI runner, for coding agents.

It is a separate package from the [CLI](../cli) and the [plugin](../plugin): it has its own dependencies and its own version, and it is **not** bundled into the plugin, so installing the plugin never installs the daemon.

## Principles

These shape everything that gets built here.

- **It dials out.** The daemon opens an outbound connection to Wheelhouse. Wheelhouse never connects into your machine, so no inbound ports or tunnels are needed.
- **Your credentials stay on your machine.** Model subscriptions and API keys are never sent to Wheelhouse. The server sends jobs and receives events and results.
- **It is not a second way to write.** Whatever an agent changes on the board or map goes through Wheelhouse's normal API, with its own key, not through the daemon.
- **Work is isolated and bounded.** Each job runs in its own git worktree on its own branch, never pushes to the default branch, and runs under limits you set. You can stop any job.
- **Jobs are authenticated.** The daemon rejects anything not signed for this machine.

## Planned layout

Not created yet; this is where things will go.

```
daemon/
├── src/        the engine: stream client, job runner, worktrees, runtime adapters
├── test/
└── README.md
```

## Relationship to the rest of this repo

| Folder | What it is | Published as |
|---|---|---|
| `cli/` | the `wheelhouse` command and the Claude Code hook runner | `@shipstack/wheelhouse-cli` |
| `plugin/` | the Claude Code plugin | the `wheelhouse` plugin |
| `daemon/` | the agent daemon (this folder) | `@shipstack/wheelhouse-daemon`, later |
