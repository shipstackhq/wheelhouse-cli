# Daemon design (MVP)

Status: agreed for the MVP and built, 2026-10-21 (server: Wheelhouse branch `wh/daemon-mvp`). Scope is macOS and the Claude Code CLI on the user's own subscription. Linux, Windows,
local models, API-key mode and the chat agent come later; where a decision affects them it is noted.

## What the MVP proves

1. Pair a machine. It shows up in Wheelhouse by name ("Atharva's MacBook Pro") as online, with what it can run.
2. "Run on device" on an issue creates a job. The daemon clones the issue's repository, makes a worktree, runs `claude -p` with the
   chosen model, streams events to the issue, and leaves a pushed branch.
3. A running job can be canceled. The daemon never runs more jobs at once than the device's pool size (default 3).

## Ground rules

- **The daemon dials out.** Wheelhouse never connects into the machine.
- **Credentials stay local.** The daemon runs the unmodified `claude` binary that the user signed into through Anthropic's own
  flow. It never reads, stores or sends Claude credentials, and it does not use the Agent SDK with a subscription login
  (Anthropic does not allow that for third-party products; see [their terms](https://code.claude.com/docs/en/legal-and-compliance)).
  The SDK becomes an option only in a later API-key mode.
- **Not a second way to write.** The daemon reports job state. Changes to the board go through Wheelhouse's normal API.
- **Isolated and bounded.** One worktree and branch per job, never the default branch, under the user's limits.

## Transport: SSE down, POST up

- `GET /api/daemon/stream`: server-sent events to the device (job offers, cancels, settings). Reconnects resume with
  `Last-Event-ID`, the same way the dashboard's stream already works (`apps/api/src/stream`).
- `POST /api/daemon/...`: everything the device sends (hello, heartbeat, job accept and events, finish).

Why not WebSocket: the traffic is lopsided (a few commands down, a steady flow of events up), SSE already exists in Wheelhouse
with replay, and plain HTTP gets per-request auth, ordinary rate limits, logs and proxy behaviour for free. Cost: job events
are many small POSTs, so the daemon batches them (flush every ~250 ms or 50 events). If the chat agent's token streaming makes
that too chatty later, that one path can move to a streaming request; the rest stays.

The daemon reads SSE with `fetch` and a small parser, because `EventSource` (the browser-standard SSE client, copied
experimentally into Node) cannot send an `Authorization` header. The same code does what `EventSource` would: reconnect with
backoff after a drop, sending the last event id it saw.

## Pairing and auth

Built for production from the start.

**Pairing** follows the OAuth device authorization flow (RFC 8628):

1. `wheelhouse daemon pair` creates an Ed25519 key pair. The private key goes into the macOS Keychain (later: DPAPI on
   Windows, the Secret Service on Linux; a `0600` file only as a last resort).
2. The daemon calls `POST /api/daemon/pair/start` with its public key, machine name and OS. The server returns a
   `device_code`, a short `user_code` and a verification URL, and the daemon prints the code and the URL.
3. The signed-in user opens the URL, sees the code, the machine name and the OS, can rename the device,
   and approves.
4. The daemon polls `POST /api/daemon/pair/poll` with the `device_code` and receives its `device_id` and the server's
   signing public key, which it pins.

**Requests.** The device has no long-lived bearer secret. To get an access token it signs a short-lived assertion (a JWT with
`sub = device_id`, `aud`, `iat`, `exp` and a `jti`) with its private key, and exchanges it at `POST /api/daemon/token` for an
access token that lasts 15 minutes, renewed before it expires. A copied access token is worth minutes, and a copied database
row holds only a public key.

**Jobs are signed.** The server signs each offer and each cancel (device id, job id, issue, repo, model, expiry). The daemon
checks the signature against the pinned key and drops anything that fails, is expired, or is for another device. So a
compromised session or proxy on the path cannot make the machine run code.

**Revocation.** Removing a device in Wheelhouse deletes its public key, so its next token request fails and its stream closes.
The daemon then stops taking jobs, finishes or cancels the current ones (user setting), and reports that it is unpaired.
`wheelhouse daemon unpair` does the same from the machine.

**Scope.** A device belongs to one user, not a workspace, and runs jobs from every workspace the user is a member of.

## Data model (first cut, refined while testing)

`device`: id, owner (user), name, os, public key, status (`online` / `offline` / `revoked`), last seen,
capabilities (JSON, from the latest report), pool size, chat model (unused until phase 2), created.

`job`: id, device, issue, repository, model, state, branch, worktree name, error, started, finished, created by.

`job_event`: job, sequence number, at, type, payload (JSON). Shown on the issue as a live log.

### Job states

```
offered ──► accepted ──► running ──► succeeded
   │            │           ├──────► failed
   │            │           └──────► canceled
   ├──► rejected (no free slot, paused, bad offer)
   └──► expired └──► (missed heartbeats) ──► lost
```

- An offer the device does not accept in 60 s is `expired`, and the user can run it again.
- A job whose device misses heartbeats for 3 minutes is `lost`. Its branch, if pushed, is kept.
- "Run on device" always creates a new job, so an issue keeps its run history and can be run again.

## Messages

Down (SSE event types): `job.offer`, `job.cancel`, `settings.update` (pool size, and later the chat model).

Up (POST):

| Message | When |
|---|---|
| `hello` | after connecting: daemon version, capabilities, jobs it still has from before a restart |
| `heartbeat` | every 30 s: free slots, running job ids, on battery or not |
| `job.accept` / `job.reject` | answer to an offer (reject: no free slot, unknown repository, bad signature) |
| `job.events` | batched progress: phase changes, tool calls (summarised), cost and tokens |
| `job.finish` | final state, branch, commit, diff stats, error |

Phase 2 adds `chat.message` (down), and `chat.delta` and `chat.done` (up).

### Capabilities

```json
{
  "claude": { "installed": true, "version": "x.y.z", "signedIn": true, "models": ["opus", "sonnet", "haiku"] },
  "repositories": [{ "repositoryId": "…", "source": "clone" }],
  "pool": { "size": 3, "busy": 1 },
  "power": { "onBattery": false }
}
```

`signedIn` and `subscriptionType` come from `claude auth status --json`, and the version from `claude --version`: the CLI is asked,
its files are never read, and the email and organisation it also prints are not sent. `models` is the CLI's aliases (opus, sonnet,
haiku) once signed in; which of them a plan can use only shows when a run tries. Wheelhouse only offers the models listed.

## The daemon itself

```
daemon/src/
  main.mjs           start up, load config and keys, wire the parts, handle shutdown
  connection.mjs     token refresh, SSE reader with Last-Event-ID, batched POST outbox, backoff
  pairing.mjs        device flow and key storage (Keychain)
  capabilities.mjs   probe the claude CLI and known repositories
  supervisor.mjs     the pool: slots, accept or reject, cancel, restart recovery
  workspace.mjs      clones and worktrees
  runner/claude.mjs  run one job: spawn `claude -p`, parse stream-json, map to job events
  state.mjs          job state on disk under ~/.wheelhouse/daemon/state
```

### Where work happens

```
~/.wheelhouse/daemon/
  repos/<repository-id>.git           bare clone, made on the first job for a repository
  worktrees/<repository-id>/<job-id>/ one per job
  state/                              job records, so a restart can reattach or report jobs as lost
  logs/
```

A Wheelhouse project can have several repositories, so a job names one **repository**, not just a project; "Run on device"
asks which when the project has more than one. The daemon never searches the disk for repos. For a GitHub-connected
repository it clones `github.com/<full name>` into its own folder, using the user's existing git credentials. A repository
with `provider = 'local'` has no remote Wheelhouse knows, so the daemon cannot clone it: it can only run jobs there once the
user points it at a checkout with the explicit opt-in `wheelhouse daemon link <path>`. Linking is planned but not in the MVP,
so the MVP runs on GitHub-connected repositories only. A per-repository setup command (for example `pnpm install`) runs
in each new worktree before the agent starts.

Finished worktrees are removed once their branch is pushed. Failed and canceled ones are kept for a day so they can be inspected.

### Running a job

```
claude -p "<prompt built from the issue>" \
  --model <job model> \
  --output-format stream-json --verbose \
  --restricted --strict-mcp-config --settings '{"disableAllHooks":true}' \
  --tools <list> --permission-mode acceptEdits --permission-prompts none \
  --allowedTools <list> --disallowedTools <list>
```

`--restricted` ignores the user's and the repository's settings files (so none of their hooks or plugins run in a job), keeps the file
tools inside the worktree and refuses `bypassPermissions`. Not `--bare`: it skips Keychain reads, which is where a subscription
sign-in lives on macOS. `--permission-prompts none` refuses whatever would have asked. The tool lists are in `config.json` (`runner`).

- The working directory is the job's worktree, on branch `wh/<issue-key>-<slug>`.
- There is no terminal to answer permission prompts, so each job runs under a fixed policy: edits allowed inside the worktree,
  a set list of shell commands, no `--dangerously-skip-permissions`. Relaying prompts to the web UI is a later step. An allow rule
  matches one command, so a compound command (`a && b`) is refused; the daemon commits what the agent leaves, so work is not lost.
- A run is told to check in proportion to its change (no tests for a docs-only change, one package's checks for code) and to install
  dependencies only when a check needs them, because a fresh worktree has none and a full suite on a large repository takes minutes.
- Limits per job: a wall-clock timeout (45 minutes by default). Cancel sends SIGTERM to the process group, then SIGKILL after 10 s.
- When it ends (also when canceled), the daemon commits anything left uncommitted and pushes the branch if it has commits, then
  sends `job.finish`. A pushed, successful worktree is removed at once; others are kept for a day for a look.
- If one job reports a rate-limit error, the supervisor pauses new jobs for the whole pool and backs off, because the limit is
  shared by the whole subscription.

### Board updates (plumbed, off)

Agents writing to the board belong to the MCP redesign, which comes after the UI work. For the MVP the job carries a
`boardAccess` field that is always `none`, and the runner has the hook where it would add the Wheelhouse MCP server and a
job-scoped key. Status changes on the issue come only from Wheelhouse, driven by job state.

## Out of scope for the MVP

The chat agent (phase 2: one long `claude -p --resume <session> --model <current>` per message, with daemon tools only and no
file or shell access), linking existing checkouts, PRs, Linux and Windows services, local models, API-key mode with the Agent
SDK, permission prompts in the web UI.

## Open before launch (not before the MVP)

- Confirm with Anthropic that the job runner and the web chat relay are fine on Pro and Max plans, and agree to the Commercial
  Terms. The current reading is that running the user's own unmodified CLI is allowed, while volume ("ordinary, individual
  usage") and the chat relay are the gray areas.
