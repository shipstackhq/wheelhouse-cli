#!/bin/sh
# Registers the Wheelhouse daemon as a per-user LaunchAgent (macOS).
#   sh install.sh --entry /path/to/daemon/src/main.mjs [--node /path/to/node] [--dry-run]
# Nothing is written to ~/Library/LaunchAgents until the entry file exists, and --dry-run only prints the plan.
set -eu

LABEL=com.shipstack.wheelhouse-daemon
HERE=$(cd "$(dirname "$0")" && pwd)
TEMPLATE="$HERE/$LABEL.plist.template"
AGENTS="$HOME/Library/LaunchAgents"
PLIST="$AGENTS/$LABEL.plist"
LOGDIR="$HOME/.wheelhouse/daemon/logs"

ENTRY=""
NODE=$(command -v node || true)
DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --entry) ENTRY=$2; shift 2 ;;
    --node) NODE=$2; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

[ -n "$ENTRY" ] || { echo "usage: install.sh --entry <main.mjs> [--node <node>] [--dry-run]" >&2; exit 2; }
[ -f "$ENTRY" ] || { echo "refusing to install: no daemon entry file at $ENTRY (the daemon is not built yet)" >&2; exit 1; }
[ -n "$NODE" ] && [ -x "$NODE" ] || { echo "refusing to install: node not found; pass --node" >&2; exit 1; }
ENTRY_ABS=$(cd "$(dirname "$ENTRY")" && pwd)/$(basename "$ENTRY")

# Escape the characters that would break the XML: & < > and quotes.
xml() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'; }

RENDERED=$(sed \
  -e "s|__NODE__|$(xml "$NODE")|g" \
  -e "s|__ENTRY__|$(xml "$ENTRY_ABS")|g" \
  -e "s|__HOME__|$(xml "$HOME")|g" \
  -e "s|__LOGDIR__|$(xml "$LOGDIR")|g" \
  "$TEMPLATE")

if [ "$DRY" -eq 1 ]; then
  echo "would write $PLIST:"
  printf '%s\n' "$RENDERED"
  echo "would create $LOGDIR and run: launchctl bootstrap gui/$(id -u) $PLIST"
  exit 0
fi

mkdir -p "$AGENTS" "$LOGDIR"
printf '%s\n' "$RENDERED" > "$PLIST"
plutil -lint "$PLIST" >/dev/null
# Replace an earlier registration of the same label, if there is one.
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "installed $LABEL (logs in $LOGDIR)"
