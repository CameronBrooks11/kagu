# 2026-11-10-01-01-smoke

- Scenario: [01-smoke](../../scenarios/01-smoke.md)
- Run: `r_c6d85404741f46f4`, final state `completed`
- Canary occurrences in the raw log: 0

## Pass criteria

- [x] `run.admitted`: substrate `bwrap`; levels `filesystem`, `process_tree` and `credentials` are `enforced`; `network` is `unrestricted`. Evidence: seq 3, and `env.json` `levels`.
- [x] `worker.initialized` has an `agent_info` that names claude-agent-acp and its version. Evidence: seq 6, `@agentclientprotocol/claude-agent-acp` 0.101.0.
- [x] The last `run.state` is `completed`. Evidence: seq 134.
- [x] `count.sh` and `README.md` exist in the workspace, and `sh count.sh count.sh` prints one line. Evidence: `count.sh: 33`, exit 0.

## Observations

- **Sign-in:** `kagu auth claude` signed in under bwrap, through the adapter's `claude-ai-login` method. `kagu-sandbox-ready` was printed to the terminal during the login.
- **Wall time:** 24.4 s from `task.submitted` (00:17:10.510Z) to `completed` (00:17:34.895Z).
- **Permission decisions:** none.
  - There is no `permission.decided` event.
  - The log records no mode. The claude-agent-acp source starts a session in `auto` when no `defaultMode` is set, and in `auto` Claude Code approves tool calls itself. That explains why no request reached kagu.
- **The one shell command failed.**
  - Claude Code's only Bash call (seq 26 to 30) failed: `bwrap: Creating new namespace failed: nesting depth or /proc/sys/user/max_*_namespaces exceeded (ENOSPC)`.
  - Its own sandbox was on because the host's managed settings in `/etc/claude-code` enable it with no unsandboxed fallback. kagu mounts `/etc` read-only, so the worker read that policy, and kagu's `--disable-userns` stopped the nested bwrap.
  - The worker wrote both files with its Write tool, seq 39 to 49, and never ran its script.

## Verdict

finding

## Issues filed

- #22: host harness config reaches confined workers, and Claude's `auto` mode bypasses kagu's permissions.
