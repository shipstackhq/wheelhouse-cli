// How the daemon draws in a terminal (see CLI-UI.md). Zero dependencies: plain ANSI codes.
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

// The wordmark, two rows of half blocks. Only code page 437 characters, so it looks the same on every terminal.
const WORDMARK = [
  "█ █ █ █ █ █▀▀ █▀▀ █   █ █ █▀█ █ █ █▀▀ █▀▀",
  "▀▄▀▄▀ █▀█ ██▄ ██▄ █▄▄ █▀█ █▄█ █▄█ ▄██ ██▄",
];
// The wake under it: a dithered fade in, full in the middle, and out.
const wake = (width) => {
  const shades = "░▒▓█";
  return Array.from({ length: width }, (_, i) => shades[Math.min(3, Math.floor((1 - Math.abs((2 * i) / (width - 1) - 1)) * 4))]).join("");
};

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

/** The header: wordmark, wake, and what is running. One line when the terminal is narrower than the wordmark. */
export function header({ version, channel, level = detectLevel(), columns = process.stdout.columns ?? 80 } = {}) {
  const tag = ["daemon", version && `v${version}`, channel].filter(Boolean).join(" · ");
  if (level === "plain") return `wheelhouse ${tag}`;
  const width = [...WORDMARK[0]].length;
  if (columns < width + 2) return `${paint("▓▒░", level)} wheelhouse ${dim(tag)}`;
  return ["", ...WORDMARK.map((row) => paint(row, level)), `${paint(wake(width), level)}`, dim(tag.padStart(width)), ""].join("\n");
}
