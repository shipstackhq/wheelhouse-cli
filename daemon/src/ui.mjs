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

// The Shipstack pennant, rasterised from the logo at 12x8 pixels: orange, blue, yellow, green bands.
const PENNANT = ["....O.......", "...OOO......", "...OOOO.....", "..BBBOOOO...", "..BBBBBOOO..", ".YYYBBBBOOO.", ".YYYYYYBB...", "GGG........."];
const BAND = { O: [246, 80, 0, 31], B: [49, 113, 242, 34], Y: [249, 201, 51, 33], G: [34, 197, 94, 32] }; // r, g, b, ANSI 16
const fg = (c, level) => (level === "full" ? `38;2;${c[0]};${c[1]};${c[2]}` : `${c[3]}`);
const bg = (c, level) => (level === "full" ? `48;2;${c[0]};${c[1]};${c[2]}` : `${c[3] + 10}`);

/** Two pixel rows per text row: the top pixel is the ▀'s colour, the bottom one its background. Only ▀ ▄ and space, so code page 437. */
function pennant(level) {
  const rows = [];
  for (let r = 0; r < PENNANT.length; r += 2) {
    rows.push([...PENNANT[r]].map((t, c) => {
      const top = BAND[t], bottom = BAND[PENNANT[r + 1][c]];
      if (top && bottom) return `\x1b[${fg(top, level)};${bg(bottom, level)}m▀\x1b[0m`;
      if (top) return `\x1b[${fg(top, level)}m▀\x1b[0m`;
      if (bottom) return `\x1b[${fg(bottom, level)}m▄\x1b[0m`;
      return " ";
    }).join(""));
  }
  return rows;
}

/**
 * The header: the Shipstack pennant, the wordmark, its wake, and what is running. Without the pennant when the terminal is too narrow
 * for both, and one line when it is narrower than the wordmark.
 */
export function header({ version, channel, level = detectLevel(), columns = process.stdout.columns ?? 80 } = {}) {
  const tag = ["daemon", version && `v${version}`, channel].filter(Boolean).join(" · ");
  if (level === "plain") return `wheelhouse ${tag}`;
  const width = [...WORDMARK[0]].length;
  if (columns < width + 2) return `${paint("▓▒░", level)} wheelhouse ${dim(tag)}`;
  const right = [...WORDMARK.map((row) => paint(row, level)), paint(wake(width), level), dim(`by shipstack · ${tag}`.padStart(width))];
  if (columns < width + PENNANT[0].length + 4) return ["", ...right, ""].join("\n");
  const left = pennant(level);
  return ["", ...right.map((line, i) => ` ${left[i]}  ${line}`), ""].join("\n");
}
