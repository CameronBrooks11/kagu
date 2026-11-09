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
import shutil
import socket
import subprocess
import sys
import urllib.parse
from pathlib import Path

VERSION = 2
BENCH = Path(__file__).resolve().parent.parent
REPO = BENCH.parent
# A string longer than this is cut, keeping its length and digest.
CAP = 4096
TRIAL_FILES = {"events.jsonl", "env.json", "trial.md"}
TOOL_FILES = {"bench.py", "test_bench.py"}
CANARY = re.compile(r"KAGU-CANARY-[0-9A-Fa-f]+", re.IGNORECASE)
# Account and host names too common to be identifying, such as a CI runner's.
GENERIC_NAMES = {
    "admin", "codespace", "debian", "fedora", "kagu", "localhost", "root",
    "runner", "ubuntu", "user", "vscode",
}

# Patterns that identify a person or machine whoever runs this. A placeholder
# (`<user>`, `~`) never matches them.
GENERIC = [
    ("home path", re.compile(r"/(?:home|Users)/(?!<)[^/\s\"'<>]+"), "/home/<user>"),
    (
        "windows home",
        re.compile(r"[A-Za-z]:\\+(?:Users|Documents and Settings)\\+(?!<)[^\\\s\"'<>]+"),
        r"C:\\Users\\<user>",
    ),
    ("tilde home", re.compile(r"(?<![\w~])~(?!<)[A-Za-z_][\w-]*(?=/)"), "~<user>"),
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
IPV6 = re.compile(r"(?<![\w:])(?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}(?![\w:])")
# Candidate secrets: runs of URL-safe base64. `/`, `+` and `.` end a run, so
# paths and versions are not candidates.
TOKEN = re.compile(r"[A-Za-z0-9_-]{16,}")
# Identifiers a trial needs whose random part would otherwise look secret.
KEEP = re.compile(r"^(?:toolu_|sha(?:256|384|512)-)")


def entropy(s):
    counts = {c: s.count(c) for c in set(s)}
    return -sum(n / len(s) * math.log2(n / len(s)) for n in counts.values())


def secret_like(s):
    """A random-looking run: upper case, lower case and digits mixed, with
    entropy near what random text of its length has.

    Identifiers are mostly single-case words and hex is never mixed-case, so
    both are kept; hex secrets are left to gitleaks. Measured on random
    base62 and URL-safe tokens of 16 to 40 characters: about 92 to 98 percent
    caught.
    """
    if KEEP.match(s):
        return False
    if not (any(c.isupper() for c in s) and any(c.islower() for c in s) and any(c.isdigit() for c in s)):
        return False
    return entropy(s) >= 0.85 * math.log2(min(len(s), 64))


def ipv6_like(s):
    return ("::" in s or s.count(":") == 7) and s not in ("::", "::1")


def canary_dir():
    return Path(os.environ.get("KAGU_CANARY_DIR") or Path.home() / ".kagu-canary")


def planted_canary():
    """The hex of the canary `bench.py canary` planted, if any."""
    try:
        text = (canary_dir() / "secret.txt").read_text().strip()
    except OSError:
        return None
    match = CANARY.fullmatch(text)
    return text[len("KAGU-CANARY-"):].lower() if match else None


def local_identifiers():
    """This machine's identifiers, read now and never written anywhere."""
    found = []

    def add(label, value, placeholder):
        if value and len(value) >= 3 and value.lower() not in GENERIC_NAMES:
            found.append((label, value, placeholder))

    add("home", str(Path.home()), "~")
    add("user", getpass.getuser(), "<user>")
    add("host", socket.gethostname(), "<host>")
    for key, placeholder in (("user.email", "<email>"), ("user.name", "<name>")):
        add("git " + key, run(["git", "config", "--get", key]) or "", placeholder)
    canary = planted_canary()
    if canary:
        found.append(("canary", canary, "<canary>"))
    # Longest first, so a home path goes before the user name inside it.
    return sorted(found, key=lambda f: -len(f[1]))


def local_pattern(label, value):
    if label == "home":
        return re.compile(re.escape(value) + r"(?![\w.-])")
    flags = re.IGNORECASE if label == "canary" else 0
    return re.compile(r"(?<![\w.-])" + re.escape(value) + r"(?![\w-])", flags)


def normalize(text):
    """Undo the encodings a path or address can hide behind."""
    text = re.sub(r"\\u002[fF]", "/", text).replace("\\/", "/")
    return urllib.parse.unquote(text)


def redact_text(text, local):
    text = normalize(text)
    text = CANARY.sub("<canary>", text)
    for label, value, placeholder in local:
        text = local_pattern(label, value).sub(placeholder, text)
    for _, pattern, placeholder in GENERIC:
        text = pattern.sub(placeholder, text)
    text = IPV6.sub(lambda m: "<ip>" if ipv6_like(m.group()) else m.group(), text)
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
    for candidate in dict.fromkeys((text, normalize(text))):
        for label, value, _ in local:
            if local_pattern(label, value).search(candidate):
                found.append(f"local {label}")
        if CANARY.search(candidate):
            found.append("canary")
        for label, pattern, _ in GENERIC:
            if pattern.search(candidate):
                found.append(label)
        if any(ipv6_like(m.group()) for m in IPV6.finditer(candidate)):
            found.append("ipv6")
        if any(secret_like(m.group()) for m in TOKEN.finditer(candidate)):
            found.append("token")
    return list(dict.fromkeys(found))


def seal(head, body):
    """The digest over a log's header fields and its body."""
    fields = {k: v for k, v in head.items() if k != "sha256"}
    return hashlib.sha256((json.dumps(fields, sort_keys=True) + "\n" + body).encode()).hexdigest()


def header_problem(path):
    """An events.jsonl must be the redactor's output, unedited. This catches
    edits; anyone can run the tool, so it is not proof against forgery."""
    lines = path.read_text().split("\n", 1)
    try:
        head = json.loads(lines[0])
    except json.JSONDecodeError:
        return "no bench header"
    if not isinstance(head, dict) or head.get("kagu_bench") != VERSION:
        return "no bench header"
    body = lines[1] if len(lines) > 1 else ""
    if seal(head, body) != head.get("sha256"):
        return "edited after capture (hash mismatch)"
    return None


def problems_of(path, local, bench):
    """Everything wrong with one path under `bench`."""
    if path.is_symlink():
        return ["symlink (git would commit its target path)"]
    rel = path.relative_to(bench)
    if rel.parts[0] == "tools":
        if "__pycache__" in rel.parts:
            return []
        return [] if len(rel.parts) == 2 and rel.name in TOOL_FILES else ["unexpected file in tools/"]
    found = []
    try:
        text = path.read_bytes().decode("utf-8")
    except UnicodeDecodeError:
        return ["not UTF-8 text"]
    found += findings(text, local)
    if rel.parts[0] == "trials":
        if len(rel.parts) != 3 or rel.name not in TRIAL_FILES:
            found.append("a trial holds only " + ", ".join(sorted(TRIAL_FILES)))
        elif rel.name == "events.jsonl":
            problem = header_problem(path)
            if problem:
                found.append(problem)
    return found


def check(paths, local=None, bench=None):
    local = local_identifiers() if local is None else local
    bench = bench or BENCH
    if paths:
        targets = [Path(os.path.abspath(p)) for p in paths]
        # A file in a trial brings in the rest of its trial.
        for p in list(targets):
            rel = p.relative_to(bench).parts if bench in p.parents else ()
            if len(rel) == 3 and rel[0] == "trials":
                targets += [p.parent / name for name in TRIAL_FILES]
    else:
        targets = []
        for root, dirs, files in os.walk(bench):
            dirs[:] = [d for d in dirs if d != "__pycache__"]
            links = [d for d in dirs if (Path(root) / d).is_symlink()]
            targets += [Path(root) / n for n in files + links]
    failed = False
    for path in dict.fromkeys(targets):
        if bench not in path.parents:
            problems = findings(path.read_text(errors="replace"), local) if path.is_file() else []
        elif not (path.exists() or path.is_symlink()):
            problems = ["missing"]
        else:
            problems = problems_of(path, local, bench)
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
    for candidate in (os.environ.get("KAGU"), REPO / "target/release/kagu"):
        if candidate and Path(candidate).is_file():
            return str(Path(candidate).resolve())
    return shutil.which("kagu") or "kagu"


def build_info(binary):
    """The commit the checkout is at, and whether `binary` was built since."""
    commit = run(["git", "-C", str(REPO), "rev-parse", "HEAD"])
    if not commit:
        sys.exit("bench: no git commit to record; capture from a kagu checkout")
    committed = run(["git", "-C", str(REPO), "log", "-1", "--format=%ct"])
    try:
        built_after = Path(binary).stat().st_mtime >= int(committed)
    except (OSError, TypeError, ValueError):
        built_after = None
    dirty = run(["git", "-C", str(REPO), "status", "--porcelain", "--", ":!bench"])
    return {
        "kagu_commit": commit,
        "kagu_tree_dirty": bool(dirty),
        "kagu_binary": binary,
        "kagu_binary_built_after_commit": built_after,
    }


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


def next_trial(trials, today, slug):
    numbers = [
        int(p.name[len(today) + 1:][:2])
        for p in trials.glob(f"{today}-[0-9][0-9]-*")
    ] if trials.exists() else []
    return trials / f"{today}-{max(numbers, default=0) + 1:02d}-{slug}"


def capture(scenario, run_id, bench=BENCH, local=None):
    local = local_identifiers() if local is None else local
    source = scenario_file(scenario, bench)
    binary = kagu_binary()
    out = subprocess.run(
        [binary, "log", run_id, "--json"], capture_output=True, text=True, check=False
    )
    if out.returncode != 0:
        sys.exit(f"bench: kagu log {run_id}: {out.stderr.strip()}")
    events = [json.loads(line) for line in out.stdout.splitlines() if line.strip()]
    planted = planted_canary()
    raw = normalize(out.stdout)
    canary_seen = (
        len(re.findall(re.escape(planted), raw, re.IGNORECASE)) if planted
        else len(CANARY.findall(raw))
    )
    body = "".join(json.dumps(redact(e, local), sort_keys=True) + "\n" for e in events)
    head = {"kagu_bench": VERSION, "events": len(events), "canary_seen": canary_seen}
    head["sha256"] = seal(head, body)

    def first(kind):
        return next((e["data"] for e in events if e.get("type") == kind), {})

    admitted = first("run.admitted")
    states = [e["data"].get("to") for e in events if e.get("type") == "run.state"]
    env = redact(
        {
            **build_info(binary),
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
    trial = next_trial(bench / "trials", today, source.stem)
    trial.mkdir(parents=True)
    (trial / "events.jsonl").write_text(json.dumps(head, sort_keys=True) + "\n" + body)
    (trial / "env.json").write_text(json.dumps(env, indent=2, sort_keys=True) + "\n")
    checklist = "\n".join(f"- [ ] {c}" for c in criteria(source.read_text()))
    (trial / "trial.md").write_text(
        f"# {trial.name}\n\n"
        f"Scenario: [{source.stem}](../../scenarios/{source.name})  \n"
        f"Run: `{run_id}`, final state `{env['final_state']}`  \n"
        f"Canary occurrences in the raw log: {canary_seen}\n\n"
        f"## Pass criteria\n\n{checklist}\n\n"
        "## Observations\n\n\n\n"
        "## Verdict\n\npass | fail | finding\n\n"
        "## Issues filed\n\n"
    )
    print(trial.relative_to(REPO) if REPO in trial.parents else trial)
    return check([trial / name for name in sorted(TRIAL_FILES)], local, bench)


def canary():
    directory = canary_dir()
    directory.mkdir(mode=0o700, exist_ok=True)
    secret = directory / "secret.txt"
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
