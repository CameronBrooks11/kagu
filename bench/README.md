# Bench

Evidence from real runs: what kagu did, recorded the same way every time, and
safe to publish.

- **A scenario** (`scenarios/NN-name.md`) is fixed: setup, the exact prompt,
  and pass criteria. Criteria are written against what the event log records,
  before the scenario's first trial. Changing them is a commit of its own,
  never part of a trial.
- **A trial** (`trials/YYYY-MM-DD-NN-scenario/`) is one run of one scenario.

## Running a trial

1. Start the host (`kagu serve`), follow the scenario's setup, and run its
   prompt with `kagu run`.
2. When the run has ended, capture it:

   ```sh
   just trial <scenario> <run>        # for example: just trial 01 r_4f9c3b582bb7aa77
   ```

   This writes the trial directory and checks it:
   - `events.jsonl`: the run's log from `kagu log <run> --json`, redacted.
     Its first line records the redactor version, the event count, how often
     the canary appeared before redaction, and a sha256 over those fields and
     the rest of the file. An edit after capture fails the gate; anyone can
     run the tool, so the hash catches edits, not forgery.
   - `env.json`: the kagu commit, whether the tree outside `bench/` was dirty,
     which kagu binary ran and whether it was built after that commit, the
     kernel, bwrap's version, the adapter's `agent_info`, and the run's
     substrate, levels and final state. Nothing is typed by hand. Capture
     refuses to run outside a git checkout.
   - `trial.md`: the scenario's criteria as a checklist.
3. Fill in `trial.md`: tick each criterion with the evidence for it (the
   event's `seq`, or the command and its output), then write your
   observations, the verdict, and the issues filed.

Verdicts:

- **pass:** every criterion held.
- **fail:** a criterion did not hold. File an issue and link it.
- **finding:** the criteria held, but something else needs an issue.

## What is published, and the gates

A trial is committed to a public repository. Its logs hold prompts, paths and
everything the model wrote, so capture redacts them:

- **This machine's identifiers,** read when the command runs and never written
  anywhere: the home directory (written as `~`), user, host, and git name and
  email. Account names too common to identify anyone (`runner`, `root`,
  `user` and the like) are not treated as identifiers.
- **Anyone's identifiers:** home paths (Unix, macOS, Windows, and `~` followed
  by a user name), email addresses, IPv4 addresses other than 127.0.0.1 and
  0.0.0.0, IPv6 addresses other than `::1`, MAC addresses, and
  `/run/user/<uid>`. Paths hidden by `%2F` or `\u002f` are decoded first.
- **Secret-like tokens:** runs of 16 or more URL-safe base64 characters that
  mix upper case, lower case and digits, with entropy near random. On random
  tokens of 16 to 40 characters this catches about 92 to 98 percent. Hex
  digests, run ids, UUIDs, tool-call ids (`toolu_`) and SRI digests are kept.
  Pure hex is never taken, since it cannot be told from a digest.
- **The canary** that [05-confinement](scenarios/05-confinement.md) plants:
  its exact value and its bare hex in any case, plus anything shaped like
  `KAGU-CANARY-…`.
- **Long strings** over 4096 characters are cut, keeping their length and
  sha256.

Then gates check every path under `bench/`:

- **The `bench-check` pre-commit hook** fails on any local identifier or
  pattern above; on an `events.jsonl` whose hash no longer matches; on a trial
  directory holding anything but its three files; on a symlink, since git
  commits its target path; on text that is not UTF-8; and on any file in
  `tools/` other than `bench.py` and `test_bench.py`.
- **`just check` and a `bench` CI job** apply the same rules. In CI the local
  identifiers are the runner's own.

Redaction is pattern-based and catches the common shapes, not every one:
a hex secret, a short token, or a path encoded as base64 passes it. gitleaks,
which runs on every commit, is the backstop for known secret formats, and the
person committing a trial reads it first. A file the gate flags is fixed by
re-capturing or by editing the text you wrote, never by weakening the gate.

Not recorded: the Landlock ABI (only the levels it produced) and the version of
Claude Code inside the adapter (only the adapter's own `agent_info`).
