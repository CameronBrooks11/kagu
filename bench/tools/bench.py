#!/usr/bin/env python3
"""Record a kagu trial, redacted, and check that nothing identifying is committed.

    bench.py capture <scenario> <run>   write bench/trials/<date>-<NN>-<scenario>/
    bench.py canary                      plant a canary secret outside any grant
    bench.py check [FILE...]             fail if a bench file is unsafe to commit

Standard library only. See bench/README.md.
"""

import datetime
import getpass
import hashlib
import json
import math
import os
import re
import secrets
import socket
import subprocess
import sys
from pathlib import Path

VERSION = 1
BENCH = Path(__file__).resolve().parent.parent
REPO = BENCH.parent
# A string longer than this is cut, keeping its length and digest.
CAP = 4096
CANARY = re.compile(r"KAGU-CANARY-[0-9a-f]{16,}")
CANARY_DIR = Path.home() / ".kagu-canary"

# Patterns that identify a person or machine whoever runs this. A placeholder
# (`<user>`, `~`) never matches them.
GENERIC = [
    ("home path", re.compile(r"/(?:home|Users)/(?!<)[^/\s\"'<>]+"), "/home/<user>"),
    ("email", re.compile(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)*\.[A-Za-z]{2,}"), "<email>"),
    ("mac", re.compile(r"\b(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}\b"), "<mac>"),
    (
        "ipv4",
        re.compile(
            r"(?<![\d.])(?!127\.0\.0\.1(?![\d.]))(?!0\.0\.0\.0(?![\d.]))"
            r"(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)(?![\d.])"
        ),
        "<ip>",
    ),
    ("uid path", re.compile(r"/run/user/\d+"), "/run/user/<uid>"),
]
TOKEN = re.compile(r"[A-Za-z0-9+/_-]{24,}={0,2}")


def entropy(s):
    counts = {c: s.count(c) for c in set(s)}
    return -sum(n / len(s) * math.log2(n / len(s)) for n in counts.values())


def secret_like(s):
    # Hex (sha256 digests, run ids) tops out at 4 bits a character; secrets
    # in base64 or base62 run higher.
    return entropy(s) >= 4.3 and any(c.isdigit() for c in s) and any(c.isalpha() for c in s)


def local_identifiers():
    """This machine's identifiers, read now and never written anywhere."""
    found = []

    def add(label, value, placeholder):
        if value and len(value) >= 3:
            found.append((label, value, placeholder))

    add("home", str(Path.home()), "~")
    add("user", getpass.getuser(), "<user>")
    add("host", socket.gethostname(), "<host>")
    for key, placeholder in (("user.email", "<email>"), ("user.name", "<name>")):
        try:
            value = subprocess.run(
                ["git", "config", "--get", key], capture_output=True, text=True, check=False
            ).stdout.strip()
        except OSError:
            value = ""
        add("git " + key, value, placeholder)
    # Longest first, so a home path goes before the user name inside it.
    return sorted(found, key=lambda f: -len(f[1]))


def word(value):
    return re.compile(r"(?<![\w.-])" + re.escape(value) + r"(?![\w-])")


def redact_text(text, local):
    for label, value, placeholder in local:
        if label == "home":
            text = text.replace(value, placeholder)
        else:
            text = word(value).sub(placeholder, text)
    text = CANARY.sub("<canary>", text)
    for _, pattern, placeholder in GENERIC:
        text = pattern.sub(placeholder, text)
    text = TOKEN.sub(lambda m: "<token>" if secret_like(m.group()) else m.group(), text)
    if len(text) > CAP:
        digest = hashlib.sha256(text.encode()).hexdigest()
        text = text[:CAP] + f"…[cut: {len(text)} chars, sha256 {digest}]"
    return text


def redact(value, local):
    if isinstance(value, str):
        return redact_text(value, local)
    if isinstance(value, list):
        return [redact(v, local) for v in value]
    if isinstance(value, dict):
        return {redact_text(k, local): redact(v, local) for k, v in value.items()}
    return value


def findings(text, local):
    """Why `text` is unsafe to commit; empty when it is safe."""
    found = []
    for label, value, _ in local:
        if (value in text) if label == "home" else word(value).search(text):
            found.append(f"local {label}")
    if CANARY.search(text):
        found.append("canary")
    for label, pattern, _ in GENERIC:
        if pattern.search(text):
            found.append(label)
    if any(secret_like(m.group()) for m in TOKEN.finditer(text)):
        found.append("token")
    return found


def header_problem(path):
    """An events.jsonl must be the redactor's output, unedited."""
    lines = path.read_text().split("\n", 1)
    try:
        head = json.loads(lines[0])
    except json.JSONDecodeError:
        return "no bench header"
    if head.get("kagu_bench") != VERSION:
        return "no bench header"
    body = lines[1] if len(lines) > 1 else ""
    if hashlib.sha256(body.encode()).hexdigest() != head.get("sha256"):
        return "edited after capture (hash mismatch)"
    return None


def check(paths, local=None):
    local = local_identifiers() if local is None else local
    if not paths:
        paths = [
            p
            for p in BENCH.rglob("*")
            if p.is_file() and BENCH / "tools" not in p.parents
        ]
    failed = False
    for path in map(Path, paths):
        path = path.resolve()
        if BENCH / "tools" in path.parents or not path.is_file():
            continue
        problems = findings(path.read_text(errors="replace"), local)
        if path.name == "events.jsonl":
            problem = header_problem(path)
            if problem:
                problems.append(problem)
        for problem in problems:
            failed = True
            shown = path.relative_to(REPO) if REPO in path.parents else path
            print(f"{shown}: {problem}", file=sys.stderr)
    return 1 if failed else 0


def run(argv):
    try:
        return subprocess.run(argv, capture_output=True, text=True, check=False).stdout.strip()
    except OSError:
        return None


def kagu_binary():
    for candidate in (os.environ.get("KAGU"), REPO / "target/release/kagu", "kagu"):
        if candidate and (Path(candidate).is_file() or candidate == "kagu"):
            return str(candidate)
    return "kagu"


def scenario_file(name, bench=BENCH):
    matches = sorted((bench / "scenarios").glob(f"{name}*.md"))
    if len(matches) != 1:
        sys.exit(f"bench: scenario {name!r} matches {len(matches)} files")
    return matches[0]


def criteria(text):
    section = text.split("## Pass criteria", 1)
    if len(section) < 2:
        return []
    body = section[1].split("\n## ", 1)[0]
    return [line[2:].strip() for line in body.splitlines() if line.startswith("- ")]


def capture(scenario, run_id, bench=BENCH, local=None):
    local = local_identifiers() if local is None else local
    source = scenario_file(scenario, bench)
    out = subprocess.run(
        [kagu_binary(), "log", run_id, "--json"], capture_output=True, text=True, check=False
    )
    if out.returncode != 0:
        sys.exit(f"bench: kagu log {run_id}: {out.stderr.strip()}")
    events = [json.loads(line) for line in out.stdout.splitlines() if line.strip()]
    canary_seen = sum(len(CANARY.findall(line)) for line in out.stdout.splitlines())
    body = "".join(json.dumps(redact(e, local), sort_keys=True) + "\n" for e in events)
    head = {
        "kagu_bench": VERSION,
        "sha256": hashlib.sha256(body.encode()).hexdigest(),
        "events": len(events),
        "canary_seen": canary_seen,
    }

    def first(kind):
        return next((e["data"] for e in events if e.get("type") == kind), {})

    admitted = first("run.admitted")
    states = [e["data"].get("to") for e in events if e.get("type") == "run.state"]
    dirty = run(["git", "-C", str(REPO), "status", "--porcelain", "--", ":!bench"])
    env = redact(
        {
            "kagu_commit": run(["git", "-C", str(REPO), "rev-parse", "HEAD"]),
            "kagu_tree_dirty": bool(dirty),
            "kernel": run(["uname", "-r"]),
            "bwrap": run(["bwrap", "--version"]),
            "agent_info": first("worker.initialized").get("agent_info"),
            "substrate": admitted.get("substrate"),
            "levels": admitted.get("levels"),
            "final_state": states[-1] if states else None,
            "run": run_id,
        },
        local,
    )

    today = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
    slug = source.stem
    trials = bench / "trials"
    taken = len(list(trials.glob(f"{today}-*"))) if trials.exists() else 0
    trial = trials / f"{today}-{taken + 1:02d}-{slug}"
    trial.mkdir(parents=True)
    (trial / "events.jsonl").write_text(json.dumps(head, sort_keys=True) + "\n" + body)
    (trial / "env.json").write_text(json.dumps(env, indent=2, sort_keys=True) + "\n")
    checklist = "\n".join(f"- [ ] {c}" for c in criteria(source.read_text()))
    (trial / "trial.md").write_text(
        f"# {trial.name}\n\n"
        f"Scenario: [{slug}](../../scenarios/{source.name})  \n"
        f"Run: `{run_id}`, final state `{env['final_state']}`  \n"
        f"Canary occurrences in the raw log: {canary_seen}\n\n"
        f"## Pass criteria\n\n{checklist}\n\n"
        "## Observations\n\n\n\n"
        "## Verdict\n\npass | fail | finding\n\n"
        "## Issues filed\n\n"
    )
    print(trial.relative_to(REPO) if REPO in trial.parents else trial)
    return check([trial / f for f in ("events.jsonl", "env.json", "trial.md")], local)


def canary():
    CANARY_DIR.mkdir(mode=0o700, exist_ok=True)
    secret = CANARY_DIR / "secret.txt"
    if not secret.exists():
        secret.write_text(f"KAGU-CANARY-{secrets.token_hex(16)}\n")
    print(secret)
    return 0


def main(argv):
    if len(argv) == 3 and argv[0] == "capture":
        return capture(argv[1], argv[2])
    if argv == ["canary"]:
        return canary()
    if argv and argv[0] == "check":
        return check(argv[1:])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
