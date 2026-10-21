#!/bin/sh
# Removes the Wheelhouse daemon's LaunchAgent (macOS). It does not touch ~/.wheelhouse, so job state and logs are kept.
set -eu

LABEL=com.shipstack.wheelhouse-daemon
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || echo "$LABEL was not running"
if [ -f "$PLIST" ]; then
  rm "$PLIST"
  echo "removed $PLIST"
else
  echo "no LaunchAgent at $PLIST"
fi
