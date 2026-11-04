#!/bin/sh
# Registers the Wheelhouse daemon as a per-user LaunchAgent (macOS).
#   sh install.sh --entry /path/to/daemon/src/main.mjs [--node /path/to/node] [--dry-run]    the Node daemon
#   sh install.sh --bin /path/to/wheelhouse-daemon [--dry-run]                               the Rust daemon
# Both use the same label, so running either one replaces the other: that is how you switch between them. Pairing, the Keychain key and
# ~/.wheelhouse/daemon are shared, so nothing is paired again.
# Nothing is written to ~/Library/LaunchAgents until the entry file exists, and --dry-run only prints the plan.
set -eu

LABEL=com.shipstack.wheelhouse-daemon
HERE=$(cd "$(dirname "$0")" && pwd)
AGENTS="$HOME/Library/LaunchAgents"
PLIST="$AGENTS/$LABEL.plist"
LOGDIR="$HOME/.wheelhouse/daemon/logs"

ENTRY=""
BIN=""
NODE=$(command -v node || true)
DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --entry) ENTRY=$2; shift 2 ;;
    --node) NODE=$2; shift 2 ;;
    --bin) BIN=$2; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

if [ -n "$BIN" ]; then
  [ -z "$ENTRY" ] || { echo "use either --bin (the Rust daemon) or --entry (the Node daemon), not both" >&2; exit 2; }
  [ -f "$BIN" ] && [ -x "$BIN" ] || { echo "refusing to install: no executable wheelhouse-daemon at $BIN" >&2; exit 1; }
  BIN_ABS=$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")
  TEMPLATE="$HERE/$LABEL.rust.plist.template"
else
  [ -n "$ENTRY" ] || { echo "usage: install.sh --entry <main.mjs> [--node <node>] | --bin <wheelhouse-daemon> [--dry-run]" >&2; exit 2; }
  [ -f "$ENTRY" ] || { echo "refusing to install: no daemon entry file at $ENTRY (the daemon is not built yet)" >&2; exit 1; }
  [ -n "$NODE" ] && [ -x "$NODE" ] || { echo "refusing to install: node not found; pass --node" >&2; exit 1; }
  ENTRY_ABS=$(cd "$(dirname "$ENTRY")" && pwd)/$(basename "$ENTRY")
  TEMPLATE="$HERE/$LABEL.plist.template"
fi

# Escape the characters that would break the XML: & < > and quotes.
xml() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'; }

RENDERED=$(sed \
  -e "s|__NODE__|$(xml "${NODE:-}")|g" \
  -e "s|__ENTRY__|$(xml "${ENTRY_ABS:-}")|g" \
  -e "s|__BIN__|$(xml "${BIN_ABS:-}")|g" \
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
