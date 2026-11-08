# kagu

A small, headless host that runs existing agent harnesses as workers.

kagu owns tasks, runs, policy and the event log. Harnesses (Claude Code,
Codex, OpenCode, …) own the agent loop and are reached over
[ACP](https://agentclientprotocol.com). kagu never calls a model.

Status: specification only. No code.

- [docs/spec.md](docs/spec.md) — model, invariants, protocols
- [docs/decisions.md](docs/decisions.md) — why
- [docs/prior-art.md](docs/prior-art.md) — what exists and how kagu differs

Licensed under [Apache-2.0](LICENSE).
