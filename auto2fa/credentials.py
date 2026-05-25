"""Credential storage abstraction over the macOS Keychain.

Before this module existed, auto2fa stored passwords and OTP secrets in
~/.auto2fa/passwords.json as plain text. Anyone with disk access (a
malicious VS Code extension, a leaked Time Machine backup, a roommate
on a shared Mac) could harvest the lot. This module moves those values
into the Keychain and reshapes passwords.json to keep only metadata.

Storage layout (macOS Keychain, kind = "generic password"):
    service: "auto2fa"
    account: "<host>.password"  → SSH password
    account: "<host>.otpauth"   → otpauth:// URL containing the TOTP secret

passwords.json (new schema v2):
    {
      "schema": 2,
      "hosts": {
        "k6":    { "autoConnect": true },
        "k8":    { "autoConnect": false }
      }
    }

Migration: when load_config() sees the old schema (top-level keys are
hostnames with "password" / "otpauthUrl" subkeys), it copies those
fields into the Keychain, rewrites passwords.json in schema v2, and
leaves a one-time backup at passwords.json.pre-keychain-backup. The
migration is idempotent — running twice does nothing the second time.
"""
from __future__ import annotations

import json
import logging
import os
import shutil
import time
from typing import Optional

import keyring
import keyring.errors

logger = logging.getLogger(__name__)

KEYCHAIN_SERVICE = "auto2fa"
SCHEMA_VERSION = 2


# ---------------------------------------------------------------------------
# Low-level Keychain wrappers
# ---------------------------------------------------------------------------

def _kc_get(account: str) -> Optional[str]:
    try:
        return keyring.get_password(KEYCHAIN_SERVICE, account)
    except keyring.errors.KeyringError as e:
        logger.warning(f"keyring get({account}) failed: {e}")
        return None


def _kc_set(account: str, secret: str) -> None:
    keyring.set_password(KEYCHAIN_SERVICE, account, secret)


def _kc_delete(account: str) -> None:
    try:
        keyring.delete_password(KEYCHAIN_SERVICE, account)
    except keyring.errors.PasswordDeleteError:
        pass  # already absent — fine


def get_password(host: str) -> Optional[str]:
    return _kc_get(f"{host}.password")


def get_otpauth(host: str) -> Optional[str]:
    return _kc_get(f"{host}.otpauth")


def set_credentials(host: str, password: str, otpauth_url: str) -> None:
    _kc_set(f"{host}.password", password)
    _kc_set(f"{host}.otpauth", otpauth_url)


def delete_credentials(host: str) -> None:
    _kc_delete(f"{host}.password")
    _kc_delete(f"{host}.otpauth")


# ---------------------------------------------------------------------------
# passwords.json — schema-aware load / save / migrate
# ---------------------------------------------------------------------------

def _passwords_path() -> str:
    config_path = os.environ.get("SSH_CONFIG_PATH")
    if not config_path:
        raise RuntimeError("SSH_CONFIG_PATH not set")
    return os.path.join(config_path, "passwords.json")


def load_config() -> dict:
    """Return a dict in the new schema:
        {"k6": {"autoConnect": True, "password": "...", "otpauthUrl": "..."}}

    Credentials are fetched from the Keychain on demand. Auto-migrates
    legacy plaintext format on first run.
    """
    path = _passwords_path()
    if not os.path.exists(path):
        return {}
    with open(path) as f:
        data = json.load(f)

    if not isinstance(data, dict):
        raise RuntimeError(f"passwords.json must be an object, got {type(data).__name__}")

    # Detect and migrate the v1 plaintext format.
    if data.get("schema") != SCHEMA_VERSION:
        migrated = _migrate_v1_to_v2(data, path)
        data = migrated

    out: dict = {}
    for host, meta in (data.get("hosts") or {}).items():
        pw = get_password(host)
        otp = get_otpauth(host)
        if pw is None or otp is None:
            logger.warning(
                f"[credentials] {host} missing in Keychain — host disabled. "
                "Re-add it via the Add Host wizard."
            )
            continue
        out[host] = {
            "password": pw,
            "otpauthUrl": otp,
            "autoConnect": bool(meta.get("autoConnect", meta.get("auto_connect", False))),
        }
    return out


def save_host_metadata(host: str, auto_connect: bool) -> None:
    """Write/update the (cred-less) metadata entry for `host`. Atomic
    via tmpfile + rename — interrupted write doesn't corrupt the JSON."""
    path = _passwords_path()
    try:
        with open(path) as f:
            data = json.load(f)
        if data.get("schema") != SCHEMA_VERSION:
            data = _migrate_v1_to_v2(data, path)
    except FileNotFoundError:
        data = {"schema": SCHEMA_VERSION, "hosts": {}}
    data.setdefault("hosts", {})[host] = {"autoConnect": auto_connect}
    data["schema"] = SCHEMA_VERSION
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def delete_host_metadata(host: str) -> None:
    path = _passwords_path()
    if not os.path.exists(path):
        return
    with open(path) as f:
        data = json.load(f)
    if data.get("schema") != SCHEMA_VERSION:
        return  # don't touch legacy file
    if data.get("hosts", {}).pop(host, None) is not None:
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(data, f, indent=2)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    delete_credentials(host)


def _migrate_v1_to_v2(legacy: dict, path: str) -> dict:
    """Move passwords + otpauth URLs into the Keychain, rewrite JSON in
    v2 format. Leaves a one-time backup at <path>.pre-keychain-backup."""
    logger.info("[credentials] migrating passwords.json → Keychain (schema v1 → v2)")
    backup = path + ".pre-keychain-backup"
    if not os.path.exists(backup):
        try:
            shutil.copy2(path, backup)
            logger.info(f"[credentials] backup saved to {backup}")
        except Exception as e:
            logger.error(f"[credentials] backup failed (refusing to migrate): {e}")
            raise

    new_hosts: dict = {}
    for host, cfg in legacy.items():
        if host in ("schema", "hosts"):
            continue  # safety: should never trigger on a true v1 file
        if not isinstance(cfg, dict):
            continue
        password = cfg.get("password", "")
        otpauth = cfg.get("otpauthUrl") or cfg.get("otpauth_url") or ""
        if not password or not otpauth:
            logger.warning(f"[credentials] {host} legacy entry missing creds — skipped")
            continue
        try:
            set_credentials(host, password, otpauth)
            new_hosts[host] = {
                "autoConnect": bool(cfg.get("autoConnect", cfg.get("auto_connect", False)))
            }
            logger.info(f"[credentials] migrated {host} into Keychain")
        except Exception as e:
            logger.error(f"[credentials] {host} migration failed (kept in legacy file): {e}")

    if not new_hosts:
        logger.warning("[credentials] no hosts migrated — refusing to overwrite passwords.json")
        return legacy

    new_data = {"schema": SCHEMA_VERSION, "hosts": new_hosts}
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(new_data, f, indent=2)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)
    logger.info(f"[credentials] migration complete — {len(new_hosts)} hosts now in Keychain")
    return new_data
