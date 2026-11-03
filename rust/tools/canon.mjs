// Reads JSON documents from stdin, one per line, and prints the daemon's canonical JSON of each, one per line.
// Used by the Rust property test (crates/wh-proto/tests/node_compat.rs).
// Splits on "\n" only: readline also breaks on U+2028, which JSON strings may contain unescaped.
import { canonicalJson } from "../../daemon/src/crypto.mjs";

let input = "";
for await (const chunk of process.stdin) input += chunk;
for (const line of input.split("\n")) {
  if (line.trim() === "") continue;
  process.stdout.write(`${canonicalJson(JSON.parse(line))}\n`);
}
