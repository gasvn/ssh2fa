"""Tests for credentials.py — schema-aware load + Keychain migration.

We test against an in-memory keyring backend so the real macOS Keychain
isn't touched. Also each test uses a fresh tmpdir as SSH_CONFIG_PATH so
filesystem state doesn't leak between cases.
"""
from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from unittest.mock import patch

import keyring
import keyring.backend


class _MemoryKeyring(keyring.backend.KeyringBackend):
    """Pure in-memory backend so tests don't poke the real Keychain."""
    priority = 1

    def __init__(self):
        super().__init__()
        self._store: dict[tuple[str, str], str] = {}

    def get_password(self, service, username):
        return self._store.get((service, username))

    def set_password(self, service, username, password):
        self._store[(service, username)] = password

    def delete_password(self, service, username):
        if (service, username) not in self._store:
            from keyring.errors import PasswordDeleteError
            raise PasswordDeleteError(f"missing {username}")
        del self._store[(service, username)]


sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from auto2fa import credentials  # noqa: E402


class _Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="a2f-cred-")
        self.old_env = os.environ.get("SSH_CONFIG_PATH")
        os.environ["SSH_CONFIG_PATH"] = self.tmp
        self.mem = _MemoryKeyring()
        self._kr_patcher = patch.object(keyring, "get_password",
                                        side_effect=self.mem.get_password)
        self._ks_patcher = patch.object(keyring, "set_password",
                                        side_effect=self.mem.set_password)
        self._kd_patcher = patch.object(keyring, "delete_password",
                                        side_effect=self.mem.delete_password)
        self._kr_patcher.start()
        self._ks_patcher.start()
        self._kd_patcher.start()

    def tearDown(self):
        import shutil
        self._kr_patcher.stop()
        self._ks_patcher.stop()
        self._kd_patcher.stop()
        shutil.rmtree(self.tmp, ignore_errors=True)
        if self.old_env is None:
            os.environ.pop("SSH_CONFIG_PATH", None)
        else:
            os.environ["SSH_CONFIG_PATH"] = self.old_env


class TestMigration(_Base):
    """Legacy plaintext passwords.json should auto-migrate on first load."""

    def _write_legacy(self, data):
        path = os.path.join(self.tmp, "passwords.json")
        with open(path, "w") as f:
            json.dump(data, f)
        return path

    def test_v1_plaintext_migrates_to_v2(self):
        path = self._write_legacy({
            "k6": {
                "password": "pw1",
                "otpauthUrl": "otpauth://totp/X:k6?secret=AAAA",
                "autoConnect": True,
            },
            "k8": {
                "password": "pw2",
                "otpauthUrl": "otpauth://totp/X:k8?secret=BBBB",
                "autoConnect": False,
            },
        })

        cfg = credentials.load_config()

        self.assertEqual(set(cfg.keys()), {"k6", "k8"})
        self.assertEqual(cfg["k6"]["password"], "pw1")
        self.assertEqual(cfg["k6"]["autoConnect"], True)
        self.assertEqual(cfg["k8"]["password"], "pw2")

        # passwords.json rewritten to v2
        with open(path) as f:
            data = json.load(f)
        self.assertEqual(data["schema"], 2)
        self.assertEqual(set(data["hosts"]), {"k6", "k8"})
        self.assertNotIn("password", data["hosts"]["k6"])

        # Keychain has the secrets
        self.assertEqual(self.mem.get_password("auto2fa", "k6.password"), "pw1")
        self.assertEqual(self.mem.get_password("auto2fa", "k6.otpauth"),
                         "otpauth://totp/X:k6?secret=AAAA")

        # Backup left
        self.assertTrue(os.path.exists(path + ".pre-keychain-backup"))

    def test_migration_is_idempotent(self):
        """Running load twice doesn't double-write / re-migrate."""
        self._write_legacy({
            "k6": {"password": "pw1",
                   "otpauthUrl": "otpauth://totp/X:k6?secret=AAAA",
                   "autoConnect": True}
        })
        credentials.load_config()
        mtime = os.path.getmtime(os.path.join(self.tmp, "passwords.json"))
        # second call — should NOT rewrite
        credentials.load_config()
        self.assertEqual(mtime, os.path.getmtime(os.path.join(self.tmp, "passwords.json")))

    def test_v2_with_missing_keychain_entry_skips_host(self):
        """If passwords.json says we have host k6 but the Keychain doesn't
        have its secrets, load_config should skip it (and warn)."""
        path = os.path.join(self.tmp, "passwords.json")
        with open(path, "w") as f:
            json.dump({"schema": 2, "hosts": {"ghost": {"autoConnect": True}}}, f)
        cfg = credentials.load_config()
        self.assertNotIn("ghost", cfg)

    def test_legacy_entry_with_missing_creds_is_skipped(self):
        """A v1 entry that doesn't have both password+otpauth shouldn't
        even attempt to migrate."""
        self._write_legacy({
            "good": {"password": "x", "otpauthUrl": "otpauth://x?secret=A"},
            "broken": {"password": "x"},  # no otpauth
        })
        cfg = credentials.load_config()
        self.assertIn("good", cfg)
        self.assertNotIn("broken", cfg)


class TestSaveDelete(_Base):
    def test_save_host_metadata_creates_file_if_missing(self):
        credentials.save_host_metadata("new-host", auto_connect=True)
        with open(os.path.join(self.tmp, "passwords.json")) as f:
            data = json.load(f)
        self.assertEqual(data["schema"], 2)
        self.assertEqual(data["hosts"]["new-host"], {"autoConnect": True})

    def test_save_host_metadata_preserves_other_hosts(self):
        credentials.save_host_metadata("a", auto_connect=True)
        credentials.save_host_metadata("b", auto_connect=False)
        with open(os.path.join(self.tmp, "passwords.json")) as f:
            data = json.load(f)
        self.assertEqual(set(data["hosts"]), {"a", "b"})

    def test_delete_credentials_removes_both(self):
        credentials.set_credentials("h", "pw", "otpauth://x?secret=A")
        self.assertEqual(self.mem.get_password("auto2fa", "h.password"), "pw")
        credentials.delete_credentials("h")
        self.assertIsNone(self.mem.get_password("auto2fa", "h.password"))
        self.assertIsNone(self.mem.get_password("auto2fa", "h.otpauth"))

    def test_delete_host_metadata_also_drops_keychain(self):
        credentials.set_credentials("h", "pw", "otpauth://x?secret=A")
        credentials.save_host_metadata("h", auto_connect=False)
        credentials.delete_host_metadata("h")
        with open(os.path.join(self.tmp, "passwords.json")) as f:
            data = json.load(f)
        self.assertNotIn("h", data["hosts"])
        self.assertIsNone(self.mem.get_password("auto2fa", "h.password"))


class TestConfigDirResolver(unittest.TestCase):
    """Regression tests for config_dir() — the boot-time "all my tunnels and
    hosts disappeared after reboot" bug.

    Root cause: at login the daemon is spawned by `zsh -lc`, which does NOT
    source .zshrc, so SSH_CONFIG_PATH is unset. load_dotenv() then picked up
    a stale `.env` pointing at another machine's path (/Users/suyc/.ssh).
    The daemon read passwords.json / tunnels.json from that non-existent
    directory, got nothing, and the UI showed an empty config — while the
    real files sat untouched in ~/.ssh. config_dir() must refuse a
    SSH_CONFIG_PATH that isn't an existing directory and fall back to ~/.ssh.
    """

    def setUp(self):
        self.old_env = os.environ.get("SSH_CONFIG_PATH")

    def tearDown(self):
        if self.old_env is None:
            os.environ.pop("SSH_CONFIG_PATH", None)
        else:
            os.environ["SSH_CONFIG_PATH"] = self.old_env

    def test_existing_dir_is_honored(self):
        tmp = tempfile.mkdtemp(prefix="a2f-cfgdir-")
        try:
            os.environ["SSH_CONFIG_PATH"] = tmp
            self.assertEqual(credentials.config_dir(), tmp)
        finally:
            import shutil
            shutil.rmtree(tmp, ignore_errors=True)

    def test_nonexistent_foreign_path_falls_back_to_ssh(self):
        # The exact failure mode: a foreign path injected by a stale .env.
        os.environ["SSH_CONFIG_PATH"] = "/Users/suyc/.ssh"
        self.assertEqual(credentials.config_dir(),
                         os.path.expanduser("~/.ssh"))

    def test_unset_falls_back_to_ssh(self):
        os.environ.pop("SSH_CONFIG_PATH", None)
        self.assertEqual(credentials.config_dir(),
                         os.path.expanduser("~/.ssh"))

    def test_empty_string_falls_back_to_ssh(self):
        os.environ["SSH_CONFIG_PATH"] = ""
        self.assertEqual(credentials.config_dir(),
                         os.path.expanduser("~/.ssh"))

    def test_tilde_path_is_expanded(self):
        os.environ["SSH_CONFIG_PATH"] = "~/.ssh"
        self.assertEqual(credentials.config_dir(),
                         os.path.expanduser("~/.ssh"))

    def test_passwords_path_routes_through_resolver(self):
        # With a poisoned env, _passwords_path() must resolve into ~/.ssh,
        # NOT the non-existent foreign directory.
        os.environ["SSH_CONFIG_PATH"] = "/Users/suyc/.ssh"
        self.assertEqual(
            credentials._passwords_path(),
            os.path.join(os.path.expanduser("~/.ssh"), "passwords.json"),
        )


if __name__ == "__main__":
    unittest.main()
