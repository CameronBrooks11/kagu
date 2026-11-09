"""Tests for bench.py: each redaction class, and each rule of the commit gate.

Identifiers here are synthetic and secrets are generated at run time, so this
file holds nothing real.
"""

import hashlib
import json
import secrets
import tempfile
import unittest
from pathlib import Path

import bench

LOCAL = sorted(
    [
        ("home", "/home/alice", "~"),
        ("user", "alice", "<user>"),
        ("host", "alicebox", "<host>"),
        ("git user.email", "alice@corp.example", "<email>"),
        ("git user.name", "Alice Liddell", "<name>"),
    ],
    key=lambda f: -len(f[1]),
)


class Redact(unittest.TestCase):
    def check_clean(self, raw, expected):
        out = bench.redact_text(raw, LOCAL)
        self.assertEqual(out, expected)
        self.assertEqual(bench.findings(out, LOCAL), [], out)

    def test_local_identifiers(self):
        self.check_clean("/home/alice/work/x.rs", "~/work/x.rs")
        self.check_clean("ran as alice on alicebox", "ran as <user> on <host>")
        self.check_clean("by Alice Liddell <alice@corp.example>", "by <name> <<email>>")

    def test_a_user_name_inside_a_word_is_left(self):
        local = [("user", "cam", "<user>")]
        self.assertEqual(bench.redact_text("camera cam", local), "camera <user>")

    def test_other_homes_emails_addresses(self):
        self.check_clean("/home/bob/.ssh/config", "/home/<user>/.ssh/config")
        self.check_clean("/Users/bob/x", "/home/<user>/x")
        self.check_clean("mail bob@example.org", "mail <email>")
        self.check_clean("lladdr 0a:1b:2c:3d:4e:5f", "lladdr <mac>")
        self.check_clean("from 192.168.1.20 to 10.0.0.1", "from <ip> to <ip>")
        self.check_clean("/run/user/1000/kagu/kagu.sock", "/run/user/<uid>/kagu/kagu.sock")

    def test_loopback_is_kept(self):
        self.check_clean("bind 127.0.0.1 and 0.0.0.0", "bind 127.0.0.1 and 0.0.0.0")

    def test_secret_like_tokens(self):
        token = secrets.token_urlsafe(32)
        self.check_clean(f"key={token}", "key=<token>")

    def test_digests_ids_and_uuids_are_kept(self):
        digest = hashlib.sha256(b"x").hexdigest()
        for kept in (digest, "r_4f9c3b582bb7aa77", "0b9e2f2e-7c1a-4b5e-9d3f-2a6c1e8b7d40"):
            self.check_clean(kept, kept)

    def test_canary(self):
        token = "KAGU-CANARY-" + secrets.token_hex(16)
        self.check_clean(f"read: {token}", "read: <canary>")

    def test_long_text_is_cut_with_its_length_and_digest(self):
        raw = "a b " * 3000
        out = bench.redact_text(raw, LOCAL)
        self.assertTrue(out.startswith(raw[: bench.CAP]))
        self.assertIn(f"[cut: {len(raw)} chars, sha256 {hashlib.sha256(raw.encode()).hexdigest()}]", out)

    def test_structures_keys_included(self):
        out = bench.redact({"/home/alice/k": ["alice", {"n": 1}]}, LOCAL)
        self.assertEqual(out, {"~/k": ["<user>", {"n": 1}]})


class Gate(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())

    def tearDown(self):
        for p in sorted(self.dir.rglob("*"), reverse=True):
            p.unlink() if p.is_file() else p.rmdir()
        self.dir.rmdir()

    def events(self, body, sha=None):
        path = self.dir / "events.jsonl"
        sha = sha or hashlib.sha256(body.encode()).hexdigest()
        path.write_text(json.dumps({"kagu_bench": bench.VERSION, "sha256": sha}) + "\n" + body)
        return path

    def test_every_class_is_found(self):
        cases = {
            "local home": "/home/alice/x",
            "local user": "user alice",
            "local host": "on alicebox",
            "local git user.email": "alice@corp.example",
            "home path": "/home/bob/x",
            "email": "bob@example.org",
            "mac": "0a:1b:2c:3d:4e:5f",
            "ipv4": "10.1.2.3",
            "uid path": "/run/user/1000",
            "token": secrets.token_urlsafe(32),
            "canary": "KAGU-CANARY-" + secrets.token_hex(16),
        }
        for label, text in cases.items():
            self.assertIn(label, bench.findings(text, LOCAL), text)

    def test_a_captured_log_passes(self):
        self.assertIsNone(bench.header_problem(self.events('{"a": 1}\n')))

    def test_an_edited_log_fails(self):
        path = self.events('{"a": 1}\n')
        path.write_text(path.read_text().replace('"a": 1', '"a": 2'))
        self.assertIn("hash mismatch", bench.header_problem(path))

    def test_a_log_without_a_header_fails(self):
        path = self.dir / "events.jsonl"
        path.write_text('{"seq": 1}\n')
        self.assertEqual(bench.header_problem(path), "no bench header")

    def test_check_fails_on_an_unsafe_file_and_passes_a_safe_one(self):
        safe = self.dir / "trial.md"
        safe.write_text("ran in ~/work as <user>\n")
        self.assertEqual(bench.check([safe], local=LOCAL), 0)
        unsafe = self.dir / "notes.md"
        unsafe.write_text("ran in /home/bob/work\n")
        self.assertEqual(bench.check([unsafe], local=LOCAL), 1)
        self.assertEqual(bench.check([self.events("{}\n", sha="0" * 64)], local=LOCAL), 1)


class Capture(unittest.TestCase):
    def test_a_trial_is_captured_redacted_and_passes_the_gate(self):
        import os
        import shutil
        import stat

        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root)
        (root / "scenarios").mkdir()
        (root / "scenarios" / "01-demo.md").write_text("# d\n\n## Pass criteria\n\n- it ran\n")
        canary = "KAGU-CANARY-" + secrets.token_hex(16)
        events = [
            {"seq": 1, "type": "run.admitted", "data": {"substrate": "bwrap", "levels": {"filesystem": "enforced"}, "home": "/home/alice/.local/state/kagu/homes/c-1"}},
            {"seq": 2, "type": "worker.initialized", "data": {"agent_info": {"name": "claude-agent-acp", "version": "0.101.0"}}},
            {"seq": 3, "type": "worker.update", "data": {"content": {"text": f"alice read {canary} from 10.1.2.3"}}},
            {"seq": 4, "type": "run.state", "data": {"to": "completed"}},
        ]
        log = root / "log.jsonl"
        log.write_text("".join(json.dumps(e) + "\n" for e in events))
        fake = root / "kagu"
        fake.write_text(f"#!/bin/sh\ncat {log}\n")
        fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        os.environ["KAGU"] = str(fake)
        self.addCleanup(os.environ.pop, "KAGU")

        self.assertEqual(bench.capture("01", "r_1", bench=root, local=LOCAL), 0)
        (trial,) = (root / "trials").iterdir()
        self.assertRegex(trial.name, r"^\d{4}-\d{2}-\d{2}-01-01-demo$")
        head, *lines = (trial / "events.jsonl").read_text().splitlines()
        head = json.loads(head)
        self.assertEqual((head["events"], head["canary_seen"]), (4, 1))
        text = "\n".join(lines)
        for leaked in ("alice", canary, "10.1.2.3"):
            self.assertNotIn(leaked, text)
        self.assertIsNone(bench.header_problem(trial / "events.jsonl"))
        env = json.loads((trial / "env.json").read_text())
        self.assertEqual(env["final_state"], "completed")
        self.assertEqual(env["agent_info"]["version"], "0.101.0")
        self.assertIn("- [ ] it ran", (trial / "trial.md").read_text())
        self.assertIn("Canary occurrences in the raw log: 1", (trial / "trial.md").read_text())


class Scenarios(unittest.TestCase):
    def test_criteria_are_read_from_their_section(self):
        text = "# s\n\n## Pass criteria\n\n- one\n- two\n\n## Notes\n\n- not this\n"
        self.assertEqual(bench.criteria(text), ["one", "two"])

    def test_every_scenario_has_criteria_and_is_clean(self):
        for path in sorted((bench.BENCH / "scenarios").glob("*.md")):
            text = path.read_text()
            self.assertTrue(bench.criteria(text), path.name)
            self.assertEqual(bench.findings(text, []), [], path.name)


if __name__ == "__main__":
    unittest.main()
