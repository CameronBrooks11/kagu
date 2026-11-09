# 04 recovery: a host that dies mid-run

When the host stops while a run is working, does the run end `interrupted` on
restart, with its worker gone?

## Setup

Start the prompt below. Once `kagu watch <run>` shows `working`, stop the host
with Ctrl-C in its terminal, then start it again with `kagu serve`.

## Prompt

```text
Read every file under /usr/share/doc one at a time and write a one-line
summary of each to summaries.txt.
```

## Pass criteria

- Before the restart, `ps -p <pid>` with the pid from `worker.spawned` finds no process: the worker died with the host.
- After the restart, the run's last `run.state` is `interrupted`.
- `kagu watch <run>` started after the restart prints the run and exits.

## Record

- How long after Ctrl-C the worker was gone, if it was not immediate.
