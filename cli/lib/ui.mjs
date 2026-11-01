// How the wheelhouse CLI and daemon draw in a terminal (see ../CLI-UI.md). Zero dependencies: plain ANSI codes.
//
//   full   Unicode, 24-bit colour            (macOS, Linux desktops and SSH, Windows Terminal, VS Code)
//   basic  code page 437 only, 16 colours    (old Windows console, Linux text console)
//   plain  no colour, ASCII                  (pipes, files, the service log, NO_COLOR)

/** The output level for this process. `--plain` and NO_COLOR win; FORCE_COLOR turns colour on for a non-terminal. */
export function detectLevel({ env = process.env, stream = process.stdout, platform = process.platform, argv = process.argv } = {}) {
  if (argv.includes("--plain") || "NO_COLOR" in env || env.TERM === "dumb") return "plain";
  if (!stream.isTTY && !env.FORCE_COLOR) return "plain";
  const unicode =
    platform === "win32"
      ? Boolean(env.WT_SESSION || env.TERM_PROGRAM === "vscode")
      : env.TERM !== "linux" && /utf-?8/i.test(env.LC_ALL || env.LC_CTYPE || env.LANG || "UTF-8");
  return unicode ? "full" : "basic";
}

// Status symbols: [full, basic and plain, colour]. One meaning each.
const SYMBOLS = { ok: ["✓", "+", 32], fail: ["✗", "x", 31], warn: ["!", "!", 33], on: ["●", "*", 36], off: ["○", "o", 2], arrow: ["→", ">", 2] };

/** A status symbol for this output level: coloured Unicode on full, coloured ASCII on basic, bare ASCII on plain. */
export function sym(name, level = detectLevel()) {
  const [unicode, ascii, colour] = SYMBOLS[name];
  return level === "plain" ? ascii : `\x1b[${colour}m${level === "full" ? unicode : ascii}\x1b[0m`;
}

// The wordmark, two rows of half blocks. Only code page 437 characters, so it looks the same on every terminal.
const WORDMARK = [
  "█ █ █ █ █ █▀▀ █▀▀ █   █ █ █▀█ █ █ █▀▀ █▀▀",
  "▀▄▀▄▀ █▀█ ██▄ ██▄ █▄▄ █▀█ █▄█ █▄█ ▄██ ██▄",
];

const FROM = [45, 212, 191]; // teal
const TO = [59, 130, 246]; // blue
const mix = (t) => FROM.map((a, i) => Math.round(a + (TO[i] - a) * t));
const rgb = ([r, g, b], s) => `\x1b[38;2;${r};${g};${b}m${s}\x1b[0m`;
const dim = (s) => `\x1b[2m${s}\x1b[0m`;

/** Paint a line left to right: a smooth gradient on full, cyan then blue halves on basic. */
function paint(line, level) {
  if (level === "full") return [...line].map((ch, i, all) => (ch === " " ? ch : rgb(mix(i / (all.length - 1)), ch))).join("");
  const half = Math.ceil(line.length / 2);
  return `\x1b[36m${line.slice(0, half)}\x1b[34m${line.slice(half)}\x1b[0m`;
}

/**
 * The header: the wordmark in a double frame, and below it who makes it (left) and the version (right). One line when the terminal
 * is narrower than the frame. The frame's ╔═╗║╚╝ are in code page 437, so it looks the same on every terminal.
 */
export function header({ version, channel, level = detectLevel(), columns = process.stdout.columns ?? 80 } = {}) {
  const tag = [version && `v${version}`, channel].filter(Boolean).join(" · ");
  if (level === "plain") return `wheelhouse ${tag}`;
  const width = [...WORDMARK[0]].length;
  const inner = width + 4;
  if (columns < inner + 2) return `${paint("▓▒░", level)} wheelhouse ${dim(tag)}`;
  const row = (line) => `${dim("║")}  ${line}  ${dim("║")}`;
  return [
    "",
    dim(`╔${"═".repeat(inner)}╗`),
    row(" ".repeat(width)),
    ...WORDMARK.map((line) => row(paint(line, level))),
    row(" ".repeat(width)),
    dim(`╚${"═".repeat(inner)}╝`),
    dim(`by shipstack.run${tag.padStart(inner + 2 - "by shipstack.run".length)}`),
    "",
  ].join("\n");
}
