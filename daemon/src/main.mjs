#!/usr/bin/env node
// What the background service starts (see platform/): the daemon in the foreground of its own process.
process.argv.splice(2, process.argv.length, "run");
await import("./cli.mjs");
