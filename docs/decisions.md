# Decisions

Each entry: decision, reason, what was rejected. Superseded entries stay,
marked.

## 1. A host, not a harness

kagu runs harnesses; it does not contain one. A minimal native loop, if
wanted, is a separate ACP worker.

Why: harness design churns monthly and the vendors out-build any third
party on it. Run lifecycle, confinement and record-keeping do not churn, and
no harness owns them across vendors. Keeping models out of the core makes
invariant 11 true by construction.

Rejected: a built-in loop in the core binary.

## 2. ACP v1 to workers

Why: every target harness speaks it natively or through a maintained
adapter; it already models prompts, cancellation, permission requests and
resume; JSON-RPC over stdio is the de-facto agent IPC. Pin
`protocolVersion: 1`, use stable methods only.

Rejected: a kagu worker protocol; PTY scraping (no structure, no
cancellation semantics); per-harness native drivers as the default (kept as
the escape hatch when an ACP adapter drops a needed feature).

## 3. Own client protocol, AHP sync model

JSON-RPC 2.0 over a unix socket. Snapshot plus `seq`, then streamed Events,
reconnect from last `seq`.

Why: AHP (Microsoft, 1.0, 2026-10) solved multi-client sync this way, and
staying close to it keeps an AHP façade cheap. Its full surface (filesystem
RPCs, automations, changesets) is far larger than kagu needs.

Rejected: adopting AHP wholesale now; A2A (remote HTTP agents, nothing to
add for local subprocesses — its state names are borrowed); gRPC, D-Bus,
varlink, Cap'n Proto (weight or ecosystem without benefit here).

## 4. The event log is the contract

Append-only, versioned envelope, host-wide `seq` (ordered, gaps allowed).
Worker output is stored as ACP `SessionUpdate`, unmodified; anything outside
ACP is stored opaque. SQLite in WAL mode.

Why: replay, audit, client resume and every projection fall out of one log.
ACP is a published standard every worker speaks, so it serves as the
presentation vocabulary without kagu inventing one; harness-native formats
change, and a schema that mirrored them would age with them. SQLite gives
crash-safe appends and queries without a server. Gapless numbering is not
needed for resume, only a total order, which also lets purge delete a run's
Events. Credential values are never written: references only, and injected
values are redacted from worker output before append.

Rejected: harness transcripts as the data model; a kagu-specific agent event
taxonomy; JSONL as primary store (no atomic multi-record writes, no index); a
full CQRS framework.

## 5. Actors, not replay

A run is a single-writer actor with a log. Recovery ends the run as
`interrupted`, outcome unknown; continuing is a new Run.

Why: deterministic replay (Temporal and kin) needs deterministic code. A
harness turn is one large non-deterministic, side-effecting step, so replay
recovers nothing and re-execution repeats side effects.

Rejected: durable-execution replay of agent work; automatic retry;
recording an exit the host never observed.

## 6. Commands for work, reconciliation for resources

Tasks and runs are imperative and at most once. Desired-state reconciliation
is reserved for things that converge: substrate available, harness home
present.

Why: a controller loop assumes idempotent actuators. "Task done" is not a
level the system can converge to, and re-running an agent is not idempotent.

Rejected: declaring tasks as desired state.

## 7. Enforcement only outside the worker

Grants compile to an OS sandbox, an egress proxy and withheld credentials.
Every setting is labelled `enforced` (the substrate prevents violation),
`supported` (the adapter applies it, conformance-tested), `advisory` (passed,
untested) or `unsupported`, per run, or `unrestricted` when the value asks
for no restriction; a profile can require a minimum level. A worker's
authority is whatever its process tree can reach, not what the Grant lists.

Why: a harness's permission rules are enforced by the process being
confined. Only the kernel, the proxy and absent credentials hold against a
worker that misbehaves. Saying so in the data is what makes the rest
trustworthy.

Rejected: presenting configured permissions as enforced; a single uniform
"policy" claim across adapters; a scale that reports a missing mechanism the
same as a deliberate request, or a tested adapter setting the same as an
untested one.

## 8. Checks decide verification

Execution state and verdict are separate. The state says how execution
ended; the verdict (`pass`, `fail`, `error`, `not_run`) says whether the
result was checked. Checks run under their own grant, with the workspace
read-only.

Why: agents report success they have not achieved, and mechanical checks
beat prose instructions. Some work (research, conversation, design) has no
mechanical check; it can complete without being reported as verified. A
check under the worker's grant could be altered by the worker it judges.

Rejected: agent self-report as verification; folding the verdict into the
terminal state; checks under the worker's grant.

## 9. Not in v1

Model-assisted routing between workers; parallel writers on one workspace;
remote or multi-tenant hosts; a GUI in the core; tool implementations.

Why: no caller yet, or the evidence is against it — parallel writers
compound errors, while single writers plus read-only reviewers help. Each can
be added on top without changing the model.

## 10. Rust, Apache-2.0

Why: ACP has an official Rust SDK (1.x, Apache-2.0); a long-lived async
supervisor benefits from Tokio and the type system (typestate for the run
lifecycle). Apache-2.0 matches the protocols kagu sits between and carries a
patent grant.

Rejected: C++ (more to get right by hand, fewer protocol libraries);
MPL-2.0 (reciprocity is not a goal for an integration layer).

## 11. Authentication belongs to the worker

A profile's auth is `native` (the worker's own credential store, granted as
a directory, never opened by kagu) or `reference` (an API key resolved from
a reference and injected). Sign-in runs the method the worker advertises.

Why: kagu supervises harnesses; it is not a model API client. The official
harness already implements its login, refresh and the locking between its
own processes. Reimplementing any of it would duplicate the worker, store
secrets kagu otherwise never holds, and break when the harness changes.

Rejected: a host-side credential chain or OAuth flow; reading, copying or
refreshing tokens; hiding or disabling a method the worker supports.

## 12. A harness home per profile

Each profile owns the directory the harness keeps its login and
transcripts in. Configuration is passed per run and never loaded from the
home. The user's own harness directory is used only on request. Workspace
harness configuration is not loaded unless the profile opts in.

Why: harnesses keep login and resumable transcripts in one directory, so a
home per task would mean signing in per task, and a home shared with the
user exposes their settings, user-level hooks (which run code) and history
read-write. The home is writable, so configuration loaded from it could be
planted by one run for the next. Workspace configuration can carry hooks
too; a repository is input, not configuration.

Rejected: a home per task; the user's `~/.claude` (or equivalent) by
default; loading workspace configuration by default.

## 13. Rust types are the contract source; profiles are TOML

Schemas are generated from the Rust types, committed, and checked for drift
in CI. Adding an optional field is compatible; any other change needs a new
version.

Why: the types are where the code lives, so generating schemas from them
keeps one source; committing the output keeps the contract reviewable
without trusting a schema-diff tool. TOML suits shallow profiles and avoids
YAML's implicit typing.

Rejected: hand-written JSON Schema driving the types; YAML; JSON (no
comments).

## 14. The caller provides the workspace

Why: a directory and a lock is the least mechanism. A git worktree needs
the main repository's `.git` writable, where a worker could plant hooks or
config that run outside the sandbox later. A host-made clone is safe but is
mechanism with no caller yet.

Rejected: git worktrees; host-created clones in v1.

## 15. A thread label, not a conversation object

Tasks carry an optional opaque `thread`. A conversation that spans workers
can be built on it when a client needs one, without migrating data.

Rejected: a conversation noun before any interactive client exists.

## 16. Substrate: bwrap and Landlock, built in

Linux first. The host drives an external `bwrap` (0.12 or later) and
applies Landlock itself; `srt` is an optional external substrate.

Why: `srt` is pre-1.0, runs on Node and documents fail-open cases, so it
cannot be the base of an enforcement claim. Without a user namespace there
is no substrate; that is reported, and runs that need one are rejected,
rather than falling back silently.

Rejected: `srt` as a required dependency; macOS Seatbelt in v1 (no network
namespace).

## 17. A TLA+ model of the core

Lifecycle, release, recovery and subscribe are model-checked with TLC in
CI.

Why: the failure modes worth catching are interleavings (a crash between
terminal and released, overlapping locks, the snapshot and cursor
boundary), which unit tests do not enumerate. TLA+ has the longest record
and TLC needs only a JVM.

Rejected: Quint (smaller project, though it can replay traces against Rust);
`stateright` (activity slowing); tests alone.

## 18. A small ACP client, threads not an async runtime

kagu speaks ACP as newline-delimited JSON-RPC over the worker's stdio with
its own few-hundred-line client. A run's actor is a thread that receives
everything from its worker on one channel.

Why: kagu sends `initialize`, `session/new`, `session/prompt` and
`session/cancel`, answers permission requests, and stores every update
unparsed (decision 4). That needs `serde_json`, not typed bindings that
track ACP's v2 draft, and blocking I/O on threads needs no runtime. The SDK
remains an option if kagu ever needs most of the protocol's types.

Rejected: the `agent-client-protocol` SDK and tokio for slice 1.

## 19. One host per log, over a private unix socket

`kagu serve` holds an exclusive lock on the database, binds a 0600 socket
in a 0700 directory, recovers, and only then serves. Clients use the typed
client API (`src/api.rs`, schemas under `schemas/api/v1/`).

Why: a second host on the same log would mark the first host's live runs
`interrupted` and break the single writer per run. The lock is taken before
the log is read or written, so a refused host has changed nothing. A unix
socket's file permissions are the whole access control, with no ports or
tokens.

Rejected: TCP on loopback (any local user can connect); one database per
host process without a lock (nothing stops a second one).

## 20. Tests split by layer

Run behaviour is tested by driving the library in-process. The socket and
CLI are tested against a real host, as `#[ignore]`d tests that CI runs
under `ok` and that `just test-integration` runs locally.

Why: a sandboxed agent session cannot bind a unix socket. Run semantics do
not depend on the transport, and testing them below it keeps them runnable
everywhere. The transport is still gated by CI on every change.
