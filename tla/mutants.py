"""Break kagu.tla once per property and check TLC reports the violation.

Run from the repo root: python3 mutants.py <tla2tools.jar>
"""
import hashlib, os, re, shutil, subprocess, sys, tempfile

JAR = sys.argv[1]
SRC = open("tla/kagu.tla").read()

CFG = """CONSTANTS
    Runs <- MCRuns
    Overlap <- MCOverlap
    Contained <- MCContained
    MaxCrashes = 2
INIT Init
NEXT Next
INVARIANTS TypeOK LogFirst LockExclusive WriterExclusive ClientConsistent
PROPERTIES TerminalFinal InterruptedOnRecovery
"""

MUTANTS = [
    ("baseline", None, None, None),
    ("release-uncontained", "WriterExclusive",
     '       \\/ r \\in Contained /\\ proc[r] = "gone"',
     '       \\/ proc[r] \\in {"gone", "orphans"}'),
    ("admit-ignores-lock", "LockExclusive",
     '    /\\ Ready /\\ st[r] = "submitted" /\\ ~Conflict(r)',
     '    /\\ Ready /\\ st[r] = "submitted"'),
    ("verdict-skips-log", "LogFirst",
     '    /\\ Ready /\\ st[r] = "checking"\n    /\\ Move(r, "completed")\n    /\\ UNCHANGED <<proc,',
     '    /\\ Ready /\\ st[r] = "checking"\n    /\\ st\' = [st EXCEPT ![r] = "completed"]\n    /\\ UNCHANGED <<log, proc,'),
    ("stale-snapshot", "ClientConsistent",
     "    /\\ view' = Fold(log)",
     "    /\\ view' = IF log = <<>> THEN Fold(log) ELSE Fold(SubSeq(log, 1, Len(log) - 1))"),
    ("cancel-after-failed", "TerminalFinal",
     '    /\\ Ready /\\ st[r] \\in NonTerminal\n    /\\ Move(r, "canceled")',
     '    /\\ Ready /\\ st[r] \\in NonTerminal \\cup {"failed"}\n    /\\ Move(r, "canceled")'),
    ("cancel-interrupts", "InterruptedOnRecovery",
     '    /\\ Ready /\\ st[r] \\in NonTerminal\n    /\\ Move(r, "canceled")',
     '    /\\ Ready /\\ st[r] \\in NonTerminal\n    /\\ Move(r, "interrupted")'),
]

failed = False
for name, expect, old, new in MUTANTS:
    d = tempfile.mkdtemp()
    shutil.copy("tla/MC.tla", d)
    open(os.path.join(d, "MC.cfg"), "w").write(CFG)
    text = SRC
    if old is not None:
        if SRC.count(old) != 1:
            print(f"{name}: anchor matched {SRC.count(old)} times"); failed = True; continue
        text = SRC.replace(old, new)
        if hashlib.sha256(text.encode()).hexdigest() == hashlib.sha256(SRC.encode()).hexdigest():
            print(f"{name}: BROKEN (file unchanged)"); failed = True; continue
    open(os.path.join(d, "kagu.tla"), "w").write(text)
    p = subprocess.run(["java", "-cp", JAR, "tlc2.TLC", "-workers", "auto", "MC"],
                       cwd=d, capture_output=True, text=True)
    viol = re.findall(r"(?:Invariant|Action property|Temporal properties) (\w*) ?(?:is|was|were) violated", p.stdout)
    line = next((l for l in p.stdout.splitlines() if "violated" in l), "")
    if expect is None:
        ok = p.returncode == 0
    else:
        ok = p.returncode != 0 and expect in p.stdout
    print(f"{name}: exit={p.returncode} expect={expect or 'pass'} -> {'OK' if ok else 'UNEXPECTED'} | {line.strip()}")
    if not ok:
        print(p.stdout[-3000:]); failed = True
sys.exit(1 if failed else 0)
