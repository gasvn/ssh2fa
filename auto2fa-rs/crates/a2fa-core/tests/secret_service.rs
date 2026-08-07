//! Live round-trip against this machine's real credential store.
//!
//! `#[ignore]` by design: it writes to the developer's actual keyring (a
//! throwaway account, deleted again at the end), which must never happen during
//! a plain `cargo test`. Run it deliberately when validating a platform:
//!
//! ```sh
//! SSH2FA_ALLOW_DEVELOPMENT_KEYCHAIN=1 cargo test -p a2fa-core --test secret_service -- --ignored --nocapture
//! ```
//!
//! On Linux this is the only thing that exercises the D-Bus/Secret Service path
//! end to end — the file vault has its own unit tests, and the daemon's e2e
//! script deliberately uses the file vault so it stays isolated.

#![cfg(target_os = "linux")]

use a2fa_core::creds::{platform_store, platform_store_description, SecretStore};

const PROBE: &str = "__ssh2fa_live_roundtrip_probe__";

#[test]
#[ignore = "writes to the real system credential store; run with --ignored"]
fn credential_store_roundtrips_on_this_machine() {
    let backend = platform_store_description();
    println!("backend: {backend}");
    // "No usable store" is an environment fact, not a defect: a headless box
    // has no keyring collection and is expected to use SSH2FA_VAULT=file. Say
    // so and stop, rather than reporting a red test for a supported setup.
    if backend.starts_with("unavailable") {
        println!(
            "SKIP: this machine has no usable Secret Service. \
             Re-run with SSH2FA_VAULT=file to exercise the file vault instead."
        );
        return;
    }
    let store = platform_store();

    // Start clean even if a previous run died mid-way.
    let _ = store.delete(PROBE);

    assert_eq!(
        store.get(PROBE).expect("get on a missing account must succeed"),
        None,
        "a missing account must read as None, not as an error"
    );

    let value = r#"{"version":1,"hosts":{"probe":{"password":"pw","otpauth":"JBSWY3DPEHPK3PXP"}}}"#;
    store.set(PROBE, value).expect("set must succeed");

    assert_eq!(
        store.get(PROBE).expect("get must succeed").as_deref(),
        Some(value),
        "the value read back must be byte-identical — the vault is JSON, so any \
         mangling (newline, encoding) silently destroys every credential"
    );

    // Overwriting must replace, not append or duplicate.
    let updated = r#"{"version":1,"hosts":{}}"#;
    store.set(PROBE, updated).expect("overwrite must succeed");
    assert_eq!(store.get(PROBE).unwrap().as_deref(), Some(updated));

    store.delete(PROBE).expect("delete must succeed");
    assert_eq!(store.get(PROBE).unwrap(), None, "delete must remove it");

    // Deleting again is a no-op, not an error — the daemon's host-removal path
    // relies on that when credentials were already gone.
    store.delete(PROBE).expect("a second delete must be a no-op");
}
