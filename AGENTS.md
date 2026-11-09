# AGENTS.md

## Project

kagu is a headless host that runs agent harnesses (Claude Code, Codex,
OpenCode, …) as ACP workers inside an OS sandbox, records an event log, and
reports whether results were verified. `docs/spec.md` is normative;
`docs/decisions.md` records why.

## Stack

- Rust 2024, toolchain pinned in `rust-toolchain.toml`
- TLA+ model of the core in `tla/`, checked with TLC
- `just` as the task runner; pre-commit with gitleaks

## Layout

```
src/        host
docs/       spec, decisions, prior art
tla/        TLA+ model and TLC config
```

## Commands

```sh
just setup   # rustfmt and clippy for the pinned toolchain
just fmt     # format
just check   # fmt-check + clippy (CI-equivalent)
just test    # unit tests
just tla     # model-check tla/ (about 18 minutes)
```

Run `just check && just test` before every commit.

## Conventions

- Spec first. A change to a contract (events, client API, profiles, run
  lifecycle, grants) lands in `docs/spec.md` and `docs/decisions.md` before
  or with the code.
- A change to the run lifecycle, release, recovery or subscribe updates
  `tla/kagu.tla` and passes TLC.
- Schemas are generated from the Rust types and committed; never edit them
  by hand.
- Every claimed guarantee names its level (`enforced`, `supported`,
  `advisory`, `unsupported`, or `unrestricted`).

## Constraints

- No AI attribution in commits or PRs.
- Do not add a dependency without saying what it replaces.
- A check that could not run must not pass.
- Sandbox integration tests need user namespaces. A Claude Code session has
  none, so they run in CI and in the user's terminal; sessions run unit
  tests only.
- Push without `-u`; a session cannot remove the config section it writes.
- Where `~/.cargo` is read-only (an agent sandbox), point `CARGO_HOME` at a
  writable directory before cargo downloads crates.
