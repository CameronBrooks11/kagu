# kagu

A small, headless host that runs existing agent harnesses as workers.

kagu owns tasks, runs, policy and the event log. Harnesses (Claude Code,
Codex, OpenCode, …) own the agent loop and are reached over
[ACP](https://agentclientprotocol.com). kagu never calls a model.

Status: specified and model-checked; implementation starting.

- [docs/spec.md](docs/spec.md): model, invariants, protocols
- [docs/decisions.md](docs/decisions.md): why
- [docs/prior-art.md](docs/prior-art.md): what exists and how kagu differs
- [tla/kagu.tla](tla/kagu.tla): TLA+ model of the core

To run Claude Code: install `claude-agent-acp`, copy
[examples/claude-code.toml](examples/claude-code.toml) to
`~/.config/kagu/profiles/claude.toml` and set its paths, then `kagu auth
claude` once.

```sh
cargo run -- serve                       # the host, in its own terminal
cargo run -- run --profile <file> --workspace <dir> --prompt <text>
cargo run -- watch <run>                 # from another terminal
cargo run -- cancel <run>

just check   # format and lint
just test    # tests that run anywhere
just test-integration                    # socket and sandbox tests
just tla     # model-check (needs tlc)
```

Licensed under [Apache-2.0](LICENSE).
