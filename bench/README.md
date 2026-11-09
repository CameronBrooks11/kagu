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
     the canary appeared before redaction, and a sha256 of the rest. Any edit
     after capture fails the gate.
   - `env.json`: the kagu commit and whether the tree outside `bench/` was
     dirty, the kernel, bwrap's version, the adapter's `agent_info`, and the
     run's substrate, levels and final state. Nothing is typed by hand.
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
  email.
- **Anyone's identifiers:** other home paths, email addresses, IP addresses
  other than 127.0.0.1 and 0.0.0.0, MAC addresses, and `/run/user/<uid>`.
- **Secret-like tokens:** 24 or more base64-like characters with high entropy.
  Hex digests, run ids and UUIDs are kept.
- **The canary** that [05-confinement](scenarios/05-confinement.md) plants.
- **Long strings** over 4096 characters are cut, keeping their length and
  sha256.

Then two gates check every file under `bench/` except `tools/`:

- **The `bench-check` pre-commit hook** fails on any local identifier, any of
  the patterns above, or an `events.jsonl` whose hash no longer matches.
- **`just check`,** which CI runs, applies the same rules. In CI there is no
  local identifier to look for, so only the patterns apply.

gitleaks runs on every commit as before. A file the gate flags is fixed by
re-capturing or by editing the text you wrote, never by weakening the gate.

Not recorded: the Landlock ABI (only the levels it produced) and the version of
Claude Code inside the adapter (only the adapter's own `agent_info`).
