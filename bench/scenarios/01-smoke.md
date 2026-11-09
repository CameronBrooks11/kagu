# 01 smoke: one confined task, end to end

Does a confined Claude Code run start, do a small task in its workspace, and
end `completed` with the levels kagu claims?

## Setup

A fresh git repository as the workspace:

```sh
mkdir -p ~/scratch/kagu-smoke && git -C ~/scratch/kagu-smoke init
```

## Prompt

```text
Write a POSIX shell script count.sh that prints the number of lines in each
file given as an argument, one "name: count" line per file, and a short
README.md that explains how to use it.
```

## Pass criteria

- `run.admitted`: substrate `bwrap`; levels `filesystem`, `process_tree` and `credentials` are `enforced`; `network` is `unrestricted`.
- `worker.initialized` has an `agent_info` that names claude-agent-acp and its version.
- The last `run.state` is `completed`.
- `count.sh` and `README.md` exist in the workspace, and `sh count.sh count.sh` prints one line.

## Record

- Wall time from `task.submitted` to the last `run.state`.
- Each `permission.decided`: what was asked, and the option chosen.
