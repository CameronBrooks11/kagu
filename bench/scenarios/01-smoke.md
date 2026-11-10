# 01 smoke: one confined task, end to end

Does a confined Claude Code run start, do a small task in its workspace, and
end `completed` with the levels kagu claims?

## Setup

A new, empty git repository as the workspace, never one an earlier trial
used:

```sh
mkdir -p ~/scratch && ws=$(mktemp -d ~/scratch/kagu-smoke-XXXX) && git -C "$ws" init -q && echo "$ws"
```

## Prompt

```text
Write a POSIX shell script count.sh that prints the number of lines in each
file given as an argument, one "name: count" line per file, and a short
README.md that explains how to use it. Then run it on count.sh to check it.
```

## Pass criteria

- `run.admitted`: substrate `bwrap`; levels `filesystem`, `process_tree` and `credentials` are `enforced`; `network` is `unrestricted`.
- `worker.initialized` has an `agent_info` that names claude-agent-acp and its version.
- The last `run.state` is `completed`.
- `count.sh` and `README.md` exist in the workspace, and `sh count.sh count.sh` prints one line.
- Every permission request reached kagu: at least one `permission.decided`, each answered `allow_once`, since the filesystem level is `enforced`.
- No tool result reports that a shell or a sandbox failed to start, and at least one shell call ends `completed`: a `worker.update` of kind `tool_call_update` whose `_meta.claudeCode.toolName` is `Bash` and whose `status` is `completed`.

## Record

- Wall time from `task.submitted` to the last `run.state`.
- Each `permission.decided`: what was asked, and the option chosen.
