# 03 cancel: a running turn stops on request

Does `kagu cancel` end a working run promptly and leave nothing running?

## Setup

Any workspace. Start the run, wait for its first `worker.update` in
`kagu watch`, then note the time and cancel:

```sh
date -u +%FT%T.%3NZ && kagu cancel <run>
```

## Prompt

```text
Read every file under /usr/share/doc one at a time and write a one-line
summary of each to summaries.txt.
```

## Pass criteria

- The last `run.state` is `canceled`, timestamped no more than 3 s after the time noted before `kagu cancel`.
- A `worker.exited` event is recorded.
- After the run ends, `ps -p <pid>` with the pid from `worker.spawned` finds no process.

## Record

- The time noted, and the gap to the `canceled` state.
