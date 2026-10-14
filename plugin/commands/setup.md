---
description: Link this repository to a Wheelhouse project (writes wheelhouse.json, installs git hooks, records baseline hashes)
argument-hint: <PROJECT_KEY>
---

Link this repository to Wheelhouse project `$ARGUMENTS` on `${user_config.server_url}`.

1. If no project key was given, ask the user for it (it's the short uppercase key shown on the Wheelhouse board, e.g. WH) and stop.
2. Run: `node "${CLAUDE_PLUGIN_ROOT}/scripts/wheelhouse.mjs" init --url ${user_config.server_url} --project $ARGUMENTS`
3. You do not need a second key. If it prints "No API key", that is fine: the git hooks queue commits locally and this plugin (which already has your key) sends them and the one-time baseline at the next session start. Just tell the user that. They only need `npx @shipstack/wheelhouse-cli login` in their own terminal if they also want to use the `wheelhouse` command or commit from outside Claude Code sessions with immediate reporting. Keys must not pass through the conversation.
4. Suggest committing `wheelhouse.json` and the `.gitignore` change so teammates share the link.
5. Call the `get_briefing` tool for the project and summarise it in a few lines.
