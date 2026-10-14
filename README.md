# Wheelhouse CLI and Claude Code plugin

Client-side tools for [Shipstack Wheelhouse](https://github.com/shipstackhq), the kanban board and knowledge map that a team and its AI agents share. They keep the board and map in sync with what actually changes in your repository.

This repo contains:

- **`cli/`**: the `@shipstack/wheelhouse-cli` npm package, which installs the `wheelhouse` command. It links a repo to a Wheelhouse project, reports what lands on the default branch, and runs the Claude Code hooks.
- **`plugin/`**: the Claude Code plugin. It adds the hooks, the Wheelhouse MCP server, `/wheelhouse:*` commands, and a skill.
- **`.claude-plugin/marketplace.json`**: lets you install the plugin straight from this repo.
- **`AGENT_README.md`**: a guide for AI agents connecting to Wheelhouse.

## Install the plugin (Claude Code)

```
/plugin marketplace add shipstackhq/wheelhouse-cli
/plugin install wheelhouse@shipstack
```

Claude Code asks for your Wheelhouse URL and an API key (stored in your OS keychain). Then, once per repository:

```
/wheelhouse:setup <PROJECT_KEY>
```

The URL defaults to https://wheelhouse.shipstack.run; point it elsewhere only for your own server. From a local clone you can also use `/plugin marketplace add /path/to/wheelhouse-cli`.

## Install the CLI

```
npm install -g @shipstack/wheelhouse-cli
wheelhouse init --project KEY            # add --url for a server other than https://wheelhouse.shipstack.run
```

See [cli/README.md](cli/README.md) for the full command list.

## Development

The plugin bundles a copy of the CLI script, so it works without `npm install`. After changing `cli/bin/wheelhouse.mjs`:

```
npm run sync-plugin   # copy the CLI into plugin/scripts/
npm run check         # verify the copy and validate the plugin
```

Release the CLI and the plugin together: bump `cli/package.json` and `plugin/.claude-plugin/plugin.json` to the same version. Installed plugins only update when the version changes.
