# Daemon CLI look and feel

How `wheelhouse-daemon` draws in a terminal. Zero dependencies: plain Node and ANSI codes (`src/ui.mjs`).

## Three output levels

The daemon picks one at start and never mixes them.

| Level | When | What it draws |
|---|---|---|
| **full** | a terminal that speaks Unicode: macOS, Linux desktops and SSH, Windows Terminal, VS Code | Unicode boxes and symbols, braille spinner, 24-bit colour gradient |
| **basic** | old Windows console (conhost), Linux text console (`TERM=linux`) | code page 437 only (`░▒▓█▀▄ ─│┌┐└┘`), 16 colours, `|/-\` spinner |
| **plain** | output is not a terminal (pipes, files, the service log), `NO_COLOR`, `TERM=dumb` | no colour, no animation, ASCII only |

Detection:

- `tty = process.stdout.isTTY`
- colour when `tty`, no `NO_COLOR`, and `TERM` is not `dumb`; `FORCE_COLOR` turns it on
- Unicode on macOS and Linux when the locale says UTF-8 and `TERM` is not `linux`; on Windows only under Windows Terminal (`WT_SESSION`) or VS Code (`TERM_PROGRAM=vscode`)
- `--plain` forces plain, for bug reports and screenshots

## Characters

| Use | full | basic / plain |
|---|---|---|
| Logo | `░ ▒ ▓ █ ▀ ▄` | same (they are in code page 437); plain prints the name in text |
| Panels | `╭ ─ ╮ │ ╰ ╯` | `┌ ─ ┐ │ └ ┘` / `+ - |` |
| Status | `✓ ✗ ! ● ○ →` | `+ x ! * o >` |
| Spinner | `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏` | `| / - \` / none |
| Bars | `▁ ▂ ▃ ▄ ▅ ▆ ▇ █` | `░ ▒ ▓ █` / none |

The logo uses only characters from code page 437 so it looks the same everywhere.

## Rules

1. The header (logo) shows on no command, `--help`, `--version`, and after `pair` and `install`. Never in `run`: its output is the service log.
2. One meaning per symbol and colour: green `✓` ok, red `✗` failed, yellow `!` warning, cyan `●` running, dim `○` stopped.
3. Columns line up in `status` and `doctor`.
4. Slow steps (pairing, updating, cloning) show a spinner that becomes `✓` or `✗`.
5. Every error says what to do next: `✗ not paired — run wheelhouse-daemon pair --server <url>`.
6. `--json` on read commands for scripts.
7. Fit 80 columns; the header shrinks to a one-line wordmark below 50.

## To build, in order

1. Header (logo, version, channel)
2. Status symbols and colours
3. Panels (`status`, `doctor`)
4. Spinner
5. `--json` and `--plain`
