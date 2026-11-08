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
is reserved for things that converge: substrate available, worktree present.

Why: a controller loop assumes idempotent actuators. "Task done" is not a
level the system can converge to, and re-running an agent is not idempotent.

Rejected: declaring tasks as desired state.

## 7. Enforcement only outside the worker

Grants compile to an OS sandbox, an egress proxy and withheld credentials.
Every setting is labelled `enforced` (the substrate prevents violation),
`supported` (the adapter applies it, conformance-tested), `advisory` (passed,
untested) or `unsupported`, per run; a profile can require a minimum level. A worker's authority is whatever
its process tree can reach, not what the Grant lists.

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
