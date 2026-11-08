# Prior art

Read from source and docs on 2026-11-08; none was run. kagu's claim is the
combination of four properties:

1. small headless core
2. versioned, durable, seq-replayable event log as the public contract
3. enforcement labelled per field as `enforced`, `advisory` or `unsupported`
4. verification by checks, recorded apart from completion

| Project | 1 | 2 | 3 | 4 |
| ------- | - | - | - | - |
| [Omnigent](https://github.com/omnigent-ai/omnigent) (Python, Apache-2.0) | partial: headless server, large service | partial: live-tail SSE, "does NOT replay history", no seq cursor | partial: real bwrap/seatbelt, egress and credential proxies; policies are in-process hooks, not labelled | no |
| [Rivet agents](https://github.com/rivet-dev/agents) `sandbox-agent` (Rust, Apache-2.0) | yes: single binary, optional UI | partial: SSE ids with Last-Event-ID replay from a 1024-entry in-memory ring; no server-side persistence | no: isolation is the surrounding sandbox | no |
| [claw-orchestrator](https://github.com/Enderfga/claw-orchestrator) (TypeScript, MIT) | partial: Node runtime | partial: per-run append-only `events.jsonl`; no seq replay, not a versioned contract | no: delegates to harness sandbox modes | yes: acceptance contracts ending `verified` / `refuted` / `unverified` |

No project found covers 2 and 3. Search was GitHub plus the
[awesome-agent-orchestrators](https://github.com/andyrewlee/awesome-agent-orchestrators)
list; a wider sweep was not done.

Interactive multi-agent GUIs (Conductor, Vibe Kanban, Claude Squad, Mux,
Sculptor, Paseo) drive several CLIs but have no headless contract or policy
model. coder/agentapi (PTY scraping) is deprecated.

## Borrowed

- **AHP** (Microsoft): snapshot + `seq` + reconnect sync model for clients.
- **A2A**: run state names.
- **Rivet**: envelope field `source` (an event the host made is never
  attributed to the worker) and opaque raw frames.
- **claw-orchestrator**: a verdict separate from completion; editing the
  files a check runs counts as refuting it (protected paths).
- **Omnigent**: credential proxy that injects secrets at egress rather than
  into the worker's environment — reference for the `srt` grant substrate.
- **OpenHands SDK**: immutable configuration plus append-only event log.

## Protocols considered

ACP, AHP, A2A, MCP, OpenTelemetry GenAI: see [decisions](decisions.md) 2–3.
