#!/bin/sh
# Runs the installed Wheelhouse CLI. The plugin keeps no copy of it.
if ! command -v wheelhouse >/dev/null 2>&1; then
  echo "Wheelhouse CLI not found. Install it: npm i -g @shipstack/wheelhouse-cli"
  exit 0
fi
exec wheelhouse "$@"
