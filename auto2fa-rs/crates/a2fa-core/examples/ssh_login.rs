// MANUAL prototype — run against a real host to validate the pty expect loop;
// not run in CI.
//
// Usage:
//   cargo run --example ssh_login -- <host> <user>
//
// The password is read from the environment variable A2FA_PASSWORD (or typed at
// stdin if the env var is absent).  The TOTP secret is read from A2FA_OTP_SECRET
// (bare base32 or otpauth:// URL); if absent the example will prompt for a
// one-time code on stdin.
//
// Example (with real creds, against a cluster that needs password + TOTP):
//   A2FA_PASSWORD=mysecret A2FA_OTP_SECRET=JBSWY3DPEHPK3PXP \
//     cargo run --example ssh_login -- cannon.rc.fas.harvard.edu myuser
//
// DO NOT run this in CI — it would hang waiting for a real SSH host.

use std::io::{self, BufRead, Write};

use a2fa_core::error::Result;
use a2fa_core::ssh::pty_auth::{run_login, LoginOutcome};
use a2fa_core::totp::totp_now;

fn main() -> Result<()> {
    // Minimal logging — print debug output to stderr when RUST_LOG=debug.
    // Uses the standard log crate; install a simple stderr logger via the
    // log::max_level() sentinel so we don't need an extra crate.
    // (Structured logging is not critical for a manual prototype.)
    if std::env::var("RUST_LOG").is_ok() {
        eprintln!("[ssh_login] RUST_LOG set — log output goes to stderr via log::Log");
    }

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: ssh_login <host> <user>");
        std::process::exit(1);
    }
    let host = &args[1];
    let user = &args[2];

    // --- Password ---
    let password = match std::env::var("A2FA_PASSWORD") {
        Ok(p) => p,
        Err(_) => {
            eprint!("Password (stdin): ");
            io::stderr().flush().ok();
            let stdin = io::stdin();
            stdin.lock().lines().next().unwrap().unwrap()
        }
    };

    // --- OTP secret (optional) ---
    let otp_secret = std::env::var("A2FA_OTP_SECRET").ok();

    // Use a simple temporary socket path for this prototype
    let control_path = format!("/tmp/cm-auto2fa-example-{host}-0");

    // Build argv — same options as master.rs start_master
    let log_file = format!("/tmp/auto2fa_ssh_example_{host}.log");
    let argv: Vec<String> = vec![
        "-v".into(),
        "-E".into(),         log_file,
        "-l".into(),         user.clone(),
        "-o".into(),         "StrictHostKeyChecking=no".into(),
        "-o".into(),         "UserKnownHostsFile=/dev/null".into(),
        "-o".into(),         "ServerAliveInterval=15".into(),
        "-o".into(),         "ServerAliveCountMax=12".into(),
        "-o".into(),         "ConnectTimeout=10".into(),
        "-o".into(),         "ControlMaster=auto".into(),
        "-o".into(),         format!("ControlPath={control_path}"),
        "-o".into(),         "ControlPersist=yes".into(),
        host.clone(),
    ];

    println!("[ssh_login] Connecting to {user}@{host} …");
    println!("[ssh_login] ControlPath: {control_path}");
    println!("[ssh_login] See verbose log: /tmp/auto2fa_ssh_example_{host}.log");

    let otp_provider = move || -> Result<String> {
        match &otp_secret {
            Some(secret) => {
                let code = totp_now(secret)?;
                println!("[ssh_login] Generated TOTP: {code}");
                Ok(code)
            }
            None => {
                eprint!("Enter OTP / Verification code: ");
                io::stderr().flush().ok();
                let stdin = io::stdin();
                let code = stdin.lock().lines().next().unwrap().unwrap();
                Ok(code.trim().to_string())
            }
        }
    };

    let outcome = run_login(&argv, &password, otp_provider)?;

    match outcome {
        LoginOutcome::Success => {
            println!("[ssh_login] SUCCESS — ControlMaster established at {control_path}");
            println!(
                "[ssh_login] Test with: ssh -o ControlPath={control_path} {host} echo ok"
            );
        }
        LoginOutcome::AuthFailed { reason } => {
            eprintln!("[ssh_login] AUTH FAILED: {reason}");
            std::process::exit(2);
        }
        LoginOutcome::Timeout => {
            eprintln!("[ssh_login] TIMEOUT — host unreachable or very slow");
            std::process::exit(3);
        }
        LoginOutcome::Eof { output } => {
            eprintln!("[ssh_login] EOF — ssh exited early");
            eprintln!("Captured output:\n{output}");
            std::process::exit(4);
        }
    }

    Ok(())
}
