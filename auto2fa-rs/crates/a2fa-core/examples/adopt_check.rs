//! Read-only pre-cutover check: for each host arg, show the resolved
//! ControlPath and whether `adopt_if_alive` would take over the live master
//! (i.e. zero-relogin handoff). Does NOT start a daemon and NEVER logs in —
//! it only runs `ssh -G` and `ssh -O check`.
//!
//! Usage: cargo run -p a2fa-core --example adopt_check -- k6 k7 k8 b8

use a2fa_core::ssh::control::{control_path, resolve_control_base, symlink_target_index};
use a2fa_core::ssh::master::{adopt_if_alive, PoolState, SlotStatus, POOL_SIZE};

fn main() {
    let hosts: Vec<String> = std::env::args().skip(1).collect();
    if hosts.is_empty() {
        eprintln!("usage: adopt_check <host>...");
        std::process::exit(2);
    }
    for host in &hosts {
        let base = resolve_control_base(host);
        println!("── {host}");
        println!("   base        = {}", base.display());
        if let Some(i) = symlink_target_index(host) {
            println!("   symlink → slot {i}");
        } else {
            println!("   symlink → (none)");
        }
        for i in 0..POOL_SIZE {
            println!("   slot {i} path = {}", control_path(host, i).display());
        }
        let mut pool = PoolState::new(host);
        let adopted = adopt_if_alive(&mut pool);
        if adopted {
            let ready: Vec<usize> = (0..POOL_SIZE)
                .filter(|&i| pool.slot_status[i] == SlotStatus::Ready)
                .collect();
            println!(
                "   ✅ WOULD ADOPT — ready slots {ready:?}, active_index={} (NO login)",
                pool.active_index
            );
        } else {
            println!("   ⚠️  no live master — would LOG IN (2FA)");
        }
    }
}
