"""Exercise the real Bash publisher against local GitHub/S3 CLI fixtures."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from tools.release_fixture import Publisher, signing_key, verify_signature


class ReleaseDownloadsTests(unittest.TestCase):
    def setUp(self):
        if not shutil.which("jq"):
            self.skipTest("publisher requires jq")
        self.temp = tempfile.TemporaryDirectory(prefix="release-downloads-fixture-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        # The real script, beside a public key for this fixture's signing key.
        try:
            self.signing_key, self.public_key = signing_key(self.root)
        except (OSError, RuntimeError, subprocess.CalledProcessError):
            self.skipTest("publisher requires OpenSSL with Ed25519")
        self.publisher = Publisher(self.root, self.public_key)
        self.names = ["vera20k-0.2.2-windows-x64-setup.exe", "vera20k-0.2.2-macos-arm64.dmg", "vera20k-0.2.2-linux-x64.AppImage"]
        assets = []
        for index, name in enumerate(self.names):
            data = (name + "\n").encode()
            (self.root / "assets" / name).write_bytes(data)
            assets.append({"id": index + 1, "name": name, "size": len(data), "digest": "sha256:" + hashlib.sha256(data).hexdigest(), "state": "uploaded"})
        self.config = {"tag": "v0.2.2", "commit": "a" * 40,
                       "release": {"id": 10, "tag_name": "v0.2.2", "draft": False, "prerelease": False, "assets": assets}}

    def run_publisher(self, tag="v0.2.2", signing_key=None):
        return self.publisher.run(tag, self.config, self.signing_key if signing_key is None else signing_key)

    def calls(self):
        path = self.root / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def puts(self):
        return [args[args.index("--key") + 1] for tool, args in self.calls() if tool == "aws" and "put-object" in args]

    def prior_pointer(self, version="0.2.1"):
        data = {"version": version, "tag": "v" + version, "commit": "c" * 40,
                "files": {platform: {} for platform in ("windows", "macos", "linux")}}
        (self.root / "objects/latest.json").write_text(json.dumps(data))
        return (self.root / "objects/latest.json").read_bytes()

    def test_publication_and_identical_retry_verify_all_bytes(self):
        result = self.run_publisher()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.puts()), 6)
        manifest = self.root / "objects/releases/v0.2.2/manifest.json"
        self.assertEqual(manifest.read_bytes(), (self.root / "objects/latest.json").read_bytes())
        signature = (self.root / "objects/releases/v0.2.2/manifest.json.sig").read_text()
        self.assertTrue(verify_signature(self.public_key, manifest.read_bytes(), signature, self.root))
        self.assertFalse(verify_signature(self.public_key, manifest.read_bytes() + b" ", signature, self.root))
        data = json.loads(manifest.read_text())
        self.assertEqual(set(data["files"]), {"windows", "macos", "linux"})
        for entry in data["files"].values():
            contents = (self.root / "objects" / entry["key"]).read_bytes()
            self.assertEqual(len(contents), entry["size"])
            self.assertEqual(hashlib.sha256(contents).hexdigest(), entry["sha256"])
        self.assertEqual(sum(args[:2] == ["attestation", "verify"] for tool, args in self.calls() if tool == "gh"), 3)
        for tool, args in self.calls():
            if tool != "aws" or "put-object" not in args:
                continue
            key = args[args.index("--key") + 1]
            if key == "latest.json":
                self.assertEqual(args[args.index("--cache-control") + 1], "no-cache")
            elif key.endswith(("/manifest.json", "/manifest.json.sig")):
                self.assertEqual(args[args.index("--content-disposition") + 1], "inline")
            else:
                self.assertEqual(args[args.index("--content-disposition") + 1],
                                 'attachment; filename="' + key.rsplit("/", 1)[1] + '"')
        again = self.run_publisher()
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(len(self.puts()), 6, "an identical retry must not overwrite any object")

    def test_signing_key_must_match_the_trusted_public_key(self):
        old_pointer = self.prior_pointer()
        self.assertNotEqual(self.run_publisher(signing_key="").returncode, 0)
        self.assertEqual(self.calls(), [], "a missing key stops before any remote call")
        other = Path(self.root / "other")
        other.mkdir()
        result = self.run_publisher(signing_key=signing_key(other)[0])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match update-key.pub", result.stderr)
        self.assertEqual(self.puts(), [])
        self.assertEqual((self.root / "objects/latest.json").read_bytes(), old_pointer)

    def test_release_shape_and_provenance_fail_before_storage_writes(self):
        for failure in ("draft", "extra", "digest", "attestation"):
            with self.subTest(failure=failure):
                original = json.loads(json.dumps(self.config))
                if failure == "draft":
                    self.config["release"]["draft"] = True
                elif failure == "extra":
                    self.config["release"]["assets"].append(dict(self.config["release"]["assets"][0], name="unexpected.zip"))
                elif failure == "digest":
                    self.config["tamper_name"] = self.names[-1]
                else:
                    self.config["attestation_fail"] = True
                result = self.run_publisher()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.puts(), [])
                self.config = original

    def test_existing_different_immutable_object_is_never_overwritten(self):
        old_pointer = self.prior_pointer()
        path = self.root / "objects/releases/v0.2.2" / self.names[0]
        path.parent.mkdir(parents=True)
        path.write_bytes(b"different existing release")
        result = self.run_publisher()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(path.read_bytes(), b"different existing release")
        self.assertEqual((self.root / "objects/latest.json").read_bytes(), old_pointer)
        self.assertEqual(self.puts(), [])

    def test_partial_or_corrupt_upload_never_publishes_a_manifest_pointer(self):
        for failure in ("fail_put", "corrupt_put"):
            with self.subTest(failure=failure):
                old_pointer = self.prior_pointer()
                self.config[failure] = "releases/v0.2.2/" + self.names[1]
                result = self.run_publisher()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual((self.root / "objects/latest.json").read_bytes(), old_pointer)
                self.assertNotIn("latest.json", self.puts())
                self.assertFalse((self.root / "objects/releases/v0.2.2/manifest.json").exists())
                self.config.pop(failure)

    def test_nonlatest_or_older_release_keeps_existing_pointer(self):
        for latest, previous in (("v0.2.3", "0.2.1"), ("v0.2.2", "0.2.3")):
            with self.subTest(latest=latest, previous=previous):
                self.config["latest_tag"] = latest
                old_pointer = self.prior_pointer(previous)
                result = self.run_publisher()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.root / "objects/latest.json").read_bytes(), old_pointer)
                self.assertNotIn("latest.json", self.puts())

    def test_failed_pointer_update_preserves_previous_pointer(self):
        old_pointer = self.prior_pointer()
        self.config["fail_put"] = "latest.json"
        result = self.run_publisher()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.root / "objects/latest.json").read_bytes(), old_pointer)
        self.assertTrue((self.root / "objects/releases/v0.2.2/manifest.json").exists())

    def test_concurrent_pointer_is_not_overwritten(self):
        self.prior_pointer()
        competitor = {"version": "0.2.3", "tag": "v0.2.3"}
        self.config["competing_latest"] = competitor
        result = self.run_publisher()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(json.loads((self.root / "objects/latest.json").read_text()), competitor)

    def test_untrusted_tag_and_existing_local_lock_stop_before_remote_calls(self):
        for tag in ("v0.2.2;echo nope", "v0.2.2-dev", "v00.2.2", "../latest"):
            with self.subTest(tag=tag):
                self.assertNotEqual(self.run_publisher(tag).returncode, 0)
                self.assertEqual(self.calls(), [])
        (self.root / "publish.lock").mkdir()
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertEqual(self.calls(), [])
        self.assertTrue((self.root / "publish.lock").is_dir())


if __name__ == "__main__":
    unittest.main()
