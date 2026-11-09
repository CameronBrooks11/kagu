# 02 continue: a follow-up resumes the same session

Does `kagu continue` resume the earlier conversation rather than start over?

## Setup

A run of [01-smoke](01-smoke.md) whose trial passed. Its run id is `<prev>`.

```sh
kagu continue <prev> --prompt "..."
```

## Prompt

```text
Add a -h flag to count.sh that prints a one-line usage message. Do not ask
what count.sh is; you wrote it.
```

## Pass criteria

- `task.submitted` has `continues` equal to `<prev>`, and a `thread` equal to the earlier task's thread or, when that had none, to `<prev>`.
- `worker.session.session_id` equals the one in `<prev>`'s log.
- The last `run.state` is `completed`.
- `sh count.sh -h` prints a usage line, and the transcript shows no question about what count.sh is.

## Record

- Whether the worker referred to the earlier turn unprompted.
