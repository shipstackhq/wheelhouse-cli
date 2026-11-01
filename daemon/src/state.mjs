// A small record per job in progress (state/jobs/<id>.json): enough for a restarted daemon to find a job it lost, stop the agent if
// it is still running, and tell Wheelhouse how that job ended.

import fs from "node:fs";
import path from "node:path";

export class JobRecords {
  constructor(dir) {
    this.dir = dir;
  }
  file(id) {
    return path.join(this.dir, `${id}.json`);
  }
  write(record) {
    fs.mkdirSync(this.dir, { recursive: true });
    fs.writeFileSync(this.file(record.id), JSON.stringify(record) + "\n", { mode: 0o600 });
  }
  update(id, patch) {
    const r = this.read(id);
    if (r) this.write({ ...r, ...patch });
  }
  read(id) {
    try {
      return JSON.parse(fs.readFileSync(this.file(id), "utf8"));
    } catch {
      return null;
    }
  }
  remove(id) {
    fs.rmSync(this.file(id), { force: true });
  }
  all() {
    try {
      return fs
        .readdirSync(this.dir)
        .filter((f) => f.endsWith(".json"))
        .map((f) => this.read(f.slice(0, -5)))
        .filter(Boolean);
    } catch {
      return [];
    }
  }
}
