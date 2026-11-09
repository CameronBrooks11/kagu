# 06 review: a real dependency bump, read-only

Is a confined run useful on real work? It reviews a bot's dependency PR and
recommends; a person decides and merges.

## Setup

Pick a public repository with an open Renovate or Dependabot PR whose checks
need only system tools. Clone it into scratch, switch to the PR branch, and
fetch the default branch:

```sh
git clone <repo> ~/scratch/kagu-review && cd ~/scratch/kagu-review
git switch <pr-branch> && git fetch origin
```

The confined run has no GitHub or SSH credentials, so it cannot push or merge.

## Prompt

```text
This branch bumps a dependency. Read the diff against the default branch
(git diff origin/HEAD...), find the changelog entries for the version range,
check how this repository uses anything that changed, and end with MERGE or
HOLD and your reasons. Do not modify files, commit, or push.
```

## Pass criteria

- The last `run.state` is `completed`.
- `git status --porcelain` in the workspace prints nothing, and `git log` shows no new commit.
- The last agent message ends with MERGE or HOLD and gives reasons.

## Record

- The PR, by its public URL.
- Your own decision on the PR, and whether the run's recommendation matched it.
- Any claim in the review that you found to be wrong.
