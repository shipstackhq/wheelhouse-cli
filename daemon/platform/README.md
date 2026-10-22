# Running the daemon as a background service

The daemon is an ordinary program. The operating system keeps it running, so it needs a per-user service on each platform:

| Platform | Mechanism | Runs as | Starts | Restarts on crash |
|---|---|---|---|---|
| macOS | LaunchAgent (`macos/`) | you, in your login session | at login | yes (`KeepAlive`): after any stop except being unpaired, which exits cleanly and stays stopped |
| Windows | Scheduled Task at logon (`windows/`) | you, in your session | at logon | yes (restart on failure) |

**Why per-user, not system-wide.** Claude Code and Codex subscriptions only work as the logged-in user, and those credentials must never leave the machine. A system service (a macOS LaunchDaemon, a Windows service) runs before anyone logs in and as a different account, so it cannot use them. For the same reason, Windows uses a logon task rather than a Windows service.

**What the service does not do.** It runs only while you are logged in. A sleeping laptop stops heartbeats, and the server then re-offers the job from its branch (see the design doc).

## Status

These files are boilerplate. They are templates and install scripts with no daemon behind them yet. Each install script checks that the entry point exists and stops if it does not, so nothing is registered for a program that is not there.

## Install and remove

macOS:

```sh
sh daemon/platform/macos/install.sh --entry /path/to/daemon/src/main.mjs --dry-run
sh daemon/platform/macos/install.sh --entry /path/to/daemon/src/main.mjs
sh daemon/platform/macos/uninstall.sh
```

Windows (PowerShell):

```powershell
.\daemon\platform\windows\install.ps1 -Entry C:\path\to\daemon\src\main.mjs -WhatIf
.\daemon\platform\windows\install.ps1 -Entry C:\path\to\daemon\src\main.mjs
.\daemon\platform\windows\uninstall.ps1
```

Both install scripts print exactly what they would register before they change anything when given the dry-run flag or `-WhatIf`.
