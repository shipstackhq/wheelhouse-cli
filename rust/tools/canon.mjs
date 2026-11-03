// Reads JSON documents from stdin, one per line, and prints the daemon's canonical JSON of each, one per line.
// Used by the Rust property test (crates/wh-proto/tests/node_compat.rs).
import readline from "node:readline";
import { canonicalJson } from "../../daemon/src/crypto.mjs";

const rl = readline.createInterface({ input: process.stdin });
for await (const line of rl) {
  if (line.trim() === "") continue;
  process.stdout.write(`${canonicalJson(JSON.parse(line))}\n`);
}
