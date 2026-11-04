// Size gate: fails when a release artifact is over its budget in rust/size-budgets.json (or is missing).
// Usage: node rust/tools/size-gate.mjs <dir-with-binaries> [<dir> ...]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const budgets = JSON.parse(fs.readFileSync(path.join(here, "..", "size-budgets.json"), "utf8"));
const dirs = process.argv.slice(2);
if (dirs.length === 0) {
  console.error("usage: size-gate.mjs <dir> [<dir> ...]");
  process.exit(2);
}

const kb = (n) => `${(n / 1024).toFixed(0)} KiB`;
let failed = false;
const rows = [];
for (const [name, max] of Object.entries(budgets)) {
  if (name.startsWith("_")) continue;
  const file = dirs.flatMap((d) => [path.join(d, name), path.join(d, `${name}.exe`)]).find((f) => fs.existsSync(f));
  if (!file) {
    rows.push(`| ${name} | missing | ${kb(max)} | FAIL |`);
    failed = true;
    continue;
  }
  const size = fs.statSync(file).size;
  const ok = size <= max;
  if (!ok) failed = true;
  rows.push(`| ${name} | ${kb(size)} (${size} B) | ${kb(max)} | ${ok ? "ok" : "OVER BUDGET"} |`);
}
const table = ["| artifact | size | budget | |", "|---|---|---|---|", ...rows].join("\n");
console.log(table);
if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${table}\n`);
process.exit(failed ? 1 : 0);
