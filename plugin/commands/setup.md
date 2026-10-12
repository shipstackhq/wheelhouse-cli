---
description: Link this repository to a Wheelhouse project (writes wheelhouse.json, installs git hooks, records baseline hashes)
argument-hint: <PROJECT_KEY>
---

Link this repository to Wheelhouse project `$ARGUMENTS` on `${user_config.server_url}`.

1. If no project key was given, ask the user for it (it's the short uppercase key shown on the Wheelhouse board, e.g. WH) and stop.
2. Run: `node "${CLAUDE_PLUGIN_ROOT}/scripts/wheelhouse.mjs" init --url ${user_config.server_url} --project $ARGUMENTS`
3. The command needs an API key for the git hooks and the baseline. If it prints "no API key yet", tell the user to run this once in their own terminal (keys must not pass through the conversation):
   `node "${CLAUDE_PLUGIN_ROOT}/scripts/wheelhouse.mjs" login --url ${user_config.server_url} --key wh_...`
   then re-run step 2.
4. Suggest committing `wheelhouse.json` and the `.gitignore` change so teammates share the link.
5. Call the `get_briefing` tool for the project and summarise it in a few lines.
