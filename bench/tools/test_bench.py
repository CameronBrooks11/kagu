"""Tests for bench.py: each redaction class, and each rule of the commit gate.

Identifiers here are synthetic and secrets are generated at run time, so this
file holds nothing real.
"""

import hashlib
import json
import os
import random
import secrets
import shutil
import stat
import string
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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
ALNUM = string.ascii_letters + string.digits
URLSAFE = ALNUM + "-_"
# Seeded: the token rule catches about 95 percent of random tokens, so a test
# on fresh randomness would fail now and then. These are still generated, so
# no secret-shaped literal sits in this file.
RNG = random.Random(19)


def random_token(n, alphabet=ALNUM):
    return "".join(RNG.choice(alphabet) for _ in range(n))


def tempdir(test):
    path = Path(tempfile.mkdtemp())
    test.addCleanup(shutil.rmtree, path)
    return path


class Redact(unittest.TestCase):
    def check_clean(self, raw, expected):
        out = bench.redact_text(raw, LOCAL)
        self.assertEqual(out, expected)
        self.assertEqual(bench.findings(out, LOCAL), [], out)

    def test_local_identifiers(self):
        self.check_clean("/home/alice/work/x.rs", "~/work/x.rs")
        self.check_clean("ran as alice on alicebox", "ran as <user> on <host>")
        self.check_clean("by Alice Liddell <alice@corp.example>", "by <name> <<email>>")

    def test_the_home_is_matched_at_a_path_boundary(self):
        self.check_clean("/home/alice2/x", "/home/<user>/x")

    def test_a_user_name_inside_a_word_is_left(self):
        local = [("user", "cam", "<user>")]
        self.assertEqual(bench.redact_text("camera cam", local), "camera <user>")

    def test_other_homes_emails_addresses(self):
        self.check_clean("/home/bob/.ssh/config", "/home/<user>/.ssh/config")
        self.check_clean("/Users/bob/x", "/home/<user>/x")
        self.check_clean(r"C:\Users\bob\x", r"C:\Users\<user>\x")
        self.check_clean("~bob/.ssh and ~/notes", "~<user>/.ssh and ~/notes")
        self.check_clean("mail bob@example.org", "mail <email>")
        self.check_clean("lladdr 0a:1b:2c:3d:4e:5f", "lladdr <mac>")
        self.check_clean("from 192.168.1.20 to 10.0.0.1", "from <ip> to <ip>")
        self.check_clean("via fe80::1c2:3ff:fe4d:5e6f and 2001:db8::7", "via <ip> and <ip>")
        self.check_clean("/run/user/1000/kagu/kagu.sock", "/run/user/<uid>/kagu/kagu.sock")

    def test_encoded_paths(self):
        self.check_clean("cwd=%2Fhome%2Fbob%2Fx", "cwd=/home/<user>/x")
        self.check_clean(r'{"p": "\u002fhome\u002fbob"}', '{"p": "/home/<user>"}')

    def test_loopback_times_and_rust_paths_are_kept(self):
        for kept in ("bind 127.0.0.1 and 0.0.0.0 and ::1", "at 19:49:14", "std::fs::read"):
            self.check_clean(kept, kept)

    def test_secret_like_tokens(self):
        for token in (
            random_token(43, URLSAFE),
            "ghp_" + random_token(36),
            "sk-ant-api03-" + random_token(80, URLSAFE),
            random_token(20),
        ):
            out = bench.redact_text(f"key={token}", LOCAL)
            self.assertNotIn(token, out)
            self.assertEqual(bench.findings(out, LOCAL), [], out)

    def test_ids_a_trial_needs_are_kept(self):
        for kept in (
            hashlib.sha256(b"x").hexdigest(),
            "r_4f9c3b582bb7aa77",
            "0b9e2f2e-7c1a-4b5e-9d3f-2a6c1e8b7d40",
            "toolu_01" + random_token(22),
            "sha512-" + random_token(40),
            "available_commands_update",
            "x86_64-unknown-linux-gnu",
            "kagu-sandbox-ready-v1-2026",
            "2026-11-09T19:49:14.123456Z",
        ):
            self.check_clean(kept, kept)

    def test_known_credential_shapes(self):
        for token in (
            "xoxb-" + random_token(13, string.digits) + "-" + random_token(13, string.digits) + "-" + random_token(24),
            "AKIA" + random_token(16, string.ascii_uppercase + string.digits),
            "sk-proj-" + random_token(48, URLSAFE),
            "github_pat_" + random_token(70, ALNUM + "_"),
        ):
            out = bench.redact_text(f"key={token}", LOCAL)
            self.assertNotIn(token, out)
            self.assertEqual(bench.findings(out, LOCAL), [], out)

    def test_code_and_text_a_trial_needs_are_kept(self):
        for kept in (
            "x[::2]",
            "use a::b;",
            "Add::add(x)",
            "dead::beef",
            "100%25 done",
            "a%2Bb%20c",
            "/srv/files/home/x",
        ):
            self.check_clean(kept, kept)

    def test_identifier_edges(self):
        self.check_clean("alice-cache and /tmp/alice_build", "<user>-cache and /tmp/<user>_build")
        self.check_clean("Alice and ALICE", "<user> and <user>")
        hexpart = RNG.getrandbits(128).to_bytes(16, "big").hex()
        local = LOCAL + [("canary", hexpart, "<canary>")]
        out = bench.redact_text(f"x{hexpart}_", local)
        self.assertNotIn(hexpart, out)

    def test_most_random_tokens_are_caught(self):
        caught = sum(bench.secret_like(random_token(24)) for _ in range(500))
        self.assertGreater(caught, 450)

    def test_canary_in_any_form(self):
        hexpart = secrets.token_hex(16)
        local = LOCAL + [("canary", hexpart, "<canary>")]
        for leaked in (f"KAGU-CANARY-{hexpart}", hexpart, hexpart.upper(), "KAGU-CANARY-abc"):
            out = bench.redact_text(f"read: {leaked}", local)
            self.assertNotIn(leaked.lower(), out.lower())
            self.assertEqual(bench.findings(out, local), [], out)

    def test_long_text_is_cut_with_its_length_and_digest(self):
        raw = "a b " * 3000
        out = bench.redact_text(raw, LOCAL)
        self.assertTrue(out.startswith(raw[: bench.CAP]))
        self.assertIn(f"[cut: {len(raw)} chars, sha256 {hashlib.sha256(raw.encode()).hexdigest()}]", out)

    def test_structures_keys_included(self):
        out = bench.redact({"/home/alice/k": ["alice", {"n": 1}]}, LOCAL)
        self.assertEqual(out, {"~/k": ["<user>", {"n": 1}]})


class Identifiers(unittest.TestCase):
    def test_generic_account_names_are_not_identifiers(self):
        with mock.patch("getpass.getuser", return_value="runner"), mock.patch(
            "socket.gethostname", return_value="localhost"
        ):
            labels = [label for label, _, _ in bench.local_identifiers()]
        self.assertNotIn("user", labels)
        self.assertNotIn("host", labels)

    def test_the_planted_canary_is_an_identifier(self):
        os.environ["KAGU_CANARY_DIR"] = str(tempdir(self))
        self.addCleanup(os.environ.pop, "KAGU_CANARY_DIR")
        bench.canary()
        planted = bench.planted_canary()
        self.assertIn(("canary", planted, "<canary>"), bench.local_identifiers())


class Gate(unittest.TestCase):
    def setUp(self):
        self.root = tempdir(self)
        self.trial = self.root / "trials" / "2026-01-01-01-01-demo"
        self.trial.mkdir(parents=True)

    def write_trial(self, body='{"a": 1}\n', head=None):
        head = head or {"kagu_bench": bench.VERSION, "events": 1, "canary_seen": 0}
        head = dict(head, sha256=bench.seal(head, body))
        (self.trial / "events.jsonl").write_text(json.dumps(head) + "\n" + body)
        (self.trial / "env.json").write_text("{}\n")
        (self.trial / "trial.md").write_text("ran in ~/work as <user>\n")

    def gate(self, *paths):
        return bench.check(list(paths), local=LOCAL, bench=self.root)

    def test_every_class_is_found(self):
        cases = {
            "local home": "/home/alice/x",
            "local user": "user alice",
            "local host": "on alicebox",
            "local git user.email": "alice@corp.example",
            "home path": "/home/bob/x",
            "windows home": r"C:\Users\bob",
            "tilde home": "~bob/x",
            "email": "bob@example.org",
            "mac": "0a:1b:2c:3d:4e:5f",
            "ipv4": "10.1.2.3",
            "ipv6": "fe80::1c2:3ff:fe4d:5e6f",
            "uid path": "/run/user/1000",
            "token": random_token(43, URLSAFE),
            "canary": "KAGU-CANARY-" + secrets.token_hex(16),
        }
        for label, text in cases.items():
            self.assertIn(label, bench.findings(text, LOCAL), text)
        self.assertIn("home path", bench.findings("%2Fhome%2Fbob", LOCAL))

    def test_a_captured_trial_passes(self):
        self.write_trial()
        self.assertEqual(self.gate(), 0)
        self.assertEqual(self.gate(self.trial / "trial.md"), 0)

    def test_an_edited_body_fails(self):
        self.write_trial()
        path = self.trial / "events.jsonl"
        path.write_text(path.read_text().replace('"a": 1', '"a": 2'))
        self.assertIn("hash mismatch", bench.header_problem(path))
        self.assertEqual(self.gate(self.trial / "trial.md"), 1)

    def test_an_edited_header_fails(self):
        self.write_trial(head={"kagu_bench": bench.VERSION, "events": 1, "canary_seen": 3})
        path = self.trial / "events.jsonl"
        path.write_text(path.read_text().replace('"canary_seen": 3', '"canary_seen": 0'))
        self.assertIn("hash mismatch", bench.header_problem(path))

    def test_a_log_without_a_header_fails(self):
        path = self.trial / "events.jsonl"
        path.write_text('{"seq": 1}\n')
        self.assertEqual(bench.header_problem(path), "no bench header")

    def test_a_trial_holds_only_its_three_files(self):
        self.write_trial()
        (self.trial / "events.json").write_text("{}\n")
        self.assertEqual(self.gate(), 1)
        self.assertEqual(self.gate(self.trial / "events.json"), 1)

    def test_a_missing_log_fails(self):
        self.write_trial()
        (self.trial / "events.jsonl").unlink()
        self.assertEqual(self.gate(self.trial / "trial.md"), 1)

    def test_symlinks_fail_even_dangling(self):
        self.write_trial()
        link = self.root / "notes.md"
        link.symlink_to("/home/bob/notes")
        self.assertEqual(self.gate(), 1)
        self.assertEqual(self.gate(link), 1)

    def test_text_that_is_not_utf8_fails(self):
        (self.root / "notes.md").write_bytes("/home/bob/x".encode("utf-16"))
        self.assertEqual(self.gate(), 1)

    def test_only_the_tool_files_live_in_tools(self):
        tools = self.root / "tools"
        tools.mkdir()
        (tools / "bench.py").write_text("x = 1\n")
        self.assertEqual(self.gate(), 0)
        (tools / "notes.md").write_text("/home/bob/x\n")
        self.assertEqual(self.gate(), 1)

    def test_the_tools_are_scanned_for_identity_and_secrets(self):
        tools = self.root / "tools"
        tools.mkdir()
        tool = tools / "test_bench.py"
        tool.write_text('FIXTURE = "/home/bob/x"\n')
        self.assertEqual(self.gate(), 0)
        tool.write_text('HOME = "/home/alice/x"\n')
        self.assertEqual(self.gate(), 1)
        tool.write_text(f'KEY = "ghp_{random_token(36)}"\n')
        self.assertEqual(self.gate(), 1)

    def test_bytecode_anywhere_fails(self):
        cache = self.root / "scenarios" / "__pycache__"
        cache.mkdir(parents=True)
        (cache / "notes.md").write_text("fine\n")
        self.assertEqual(self.gate(), 1)
        self.assertEqual(self.gate(cache / "notes.md"), 1)

    def test_a_symlinked_directory_fails(self):
        self.write_trial()
        (self.root / "linked").symlink_to(self.trial, target_is_directory=True)
        self.assertEqual(self.gate(), 1)

    def test_an_unsafe_file_fails_and_a_safe_one_passes(self):
        safe = self.root / "notes.md"
        safe.write_text("ran in ~/work as <user>\n")
        self.assertEqual(self.gate(safe), 0)
        safe.write_text("ran in /home/bob/work\n")
        self.assertEqual(self.gate(safe), 1)


class Capture(unittest.TestCase):
    def setUp(self):
        self.root = tempdir(self)
        (self.root / "scenarios").mkdir()
        (self.root / "scenarios" / "01-demo.md").write_text("# d\n\n## Pass criteria\n\n- it ran\n")
        self.canary_dir = self.root / "canary"
        os.environ["KAGU_CANARY_DIR"] = str(self.canary_dir)
        self.addCleanup(os.environ.pop, "KAGU_CANARY_DIR")

    def fake_kagu(self, events):
        log = self.root / "log.jsonl"
        log.write_text("".join(json.dumps(e) + "\n" for e in events))
        fake = self.root / "kagu"
        fake.write_text(f"#!/bin/sh\ncat {log}\n")
        fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        os.environ["KAGU"] = str(fake)
        self.addCleanup(os.environ.pop, "KAGU")

    def test_a_trial_is_captured_redacted_and_passes_the_gate(self):
        bench.canary()
        hexpart = bench.planted_canary()
        local = LOCAL + [("canary", hexpart, "<canary>")]
        events = [
            {"seq": 1, "type": "run.admitted", "data": {"substrate": "bwrap", "levels": {"filesystem": "enforced"}, "home": "/home/alice/.local/state/kagu/homes/c-1"}},
            {"seq": 2, "type": "worker.initialized", "data": {"agent_info": {"name": "claude-agent-acp", "version": "0.101.0"}}},
            {"seq": 3, "type": "worker.update", "data": {"content": {"text": f"alice read KAGU-CANARY-{hexpart} from 10.1.2.3"}}},
            {"seq": 4, "type": "worker.update", "data": {"content": {"text": f"the hex alone: {hexpart.upper()}"}}},
            {"seq": 5, "type": "run.state", "data": {"to": "completed"}},
        ]
        self.fake_kagu(events)
        self.assertEqual(bench.capture("01", "r_1", bench=self.root, local=local), 0)
        (trial,) = (self.root / "trials").iterdir()
        self.assertEqual(trial.name[10:], "-01-01-demo")
        head, *lines = (trial / "events.jsonl").read_text().splitlines()
        head = json.loads(head)
        self.assertEqual((head["events"], head["canary_seen"]), (5, 2))
        text = "\n".join(lines)
        for leaked in ("alice", hexpart, "10.1.2.3"):
            self.assertNotIn(leaked, text.lower())
        self.assertIsNone(bench.header_problem(trial / "events.jsonl"))
        env = json.loads((trial / "env.json").read_text())
        self.assertEqual(env["final_state"], "completed")
        self.assertEqual(env["agent_info"]["version"], "0.101.0")
        self.assertTrue(env["kagu_commit"])
        trial_md = (trial / "trial.md").read_text()
        self.assertIn("- [ ] it ran", trial_md)
        self.assertIn("Canary occurrences in the raw log: 2", trial_md)

    def test_numbering_continues_after_a_deleted_trial(self):
        trials = self.root / "trials"
        (trials / "2026-01-01-01-01-demo").mkdir(parents=True)
        (trials / "2026-01-01-03-01-demo").mkdir()
        self.assertEqual(bench.next_trial(trials, "2026-01-01", "01-demo").name, "2026-01-01-04-01-demo")
        for n in range(5, 101):
            (trials / f"2026-01-01-{n:02d}-01-demo").mkdir()
        self.assertEqual(bench.next_trial(trials, "2026-01-01", "01-demo").name, "2026-01-01-101-01-demo")

    def test_capture_refuses_without_a_commit(self):
        with mock.patch.object(bench, "run", return_value=""):
            with self.assertRaises(SystemExit):
                bench.build_info("kagu")


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
