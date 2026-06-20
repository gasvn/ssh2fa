# Direct-Mode Port Forwarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a tunnel mode that forwards a local port straight to a service on a registered host (`ssh -N -L <local>:localhost:<remote> <host>`) — no jump host, no SLURM compute node — reusing the host's warm ControlMaster so it connects instantly with no 2FA.

**Architecture:** A new persisted `direct_host: Option<String>` field on `Tunnel` marks the mode (serde default `None` = today's compute behavior). A `ForwardSpec` enum (`Compute { jump, user, node }` | `Direct { host }`) threads the two modes through both start-workers. The two pure decision functions (`tunnel_action`, `should_autostart`) gain an `is_direct` gate that suppresses the SLURM-only squeue checks and the node-required boot gate. Swift gets a target picker in the create sheet and a direct-aware tunnel row.

**Tech Stack:** Rust (a2fa-core, a2fa-daemon; `cargo test`), Swift/SwiftUI (auto2fa-mac; Xcode build).

**Reference spec:** `docs/superpowers/specs/2026-06-19-direct-mode-port-forwarding-design.md`

---

## Ordering rationale

Tasks are ordered so the tree **compiles and tests pass after every task**:
1. Model field first (it's `#[serde(default)]`, breaks nothing).
2. `forward.rs` pure functions + `ForwardSpec` (additive, nothing calls them yet).
3. Decision-function gates (`tunnel_action`, `should_autostart`) — signature change + all call sites + tests in one task (won't compile half-done).
4. Workers switch to `ForwardSpec` — signature change + both call sites (IPC handler, maintenance) in one task.
5. Daemon start branches + `tunnel_add` + snapshot + `TunnelSnapshot`.
6. Swift model + IPC + AppState (compiles independently).
7. Swift UI (create sheet + row).

Rust working directory for all `cargo` commands: `/Users/shgao/logs/auto2fa_dev/auto2fa-rs`.
Swift project: `/Users/shgao/logs/auto2fa_dev/auto2fa-mac`.

---

## Task 1: Add `direct_host` field to the Tunnel model

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/model/tunnel.rs`

The field is `#[serde(default)]` so old `tunnels.json` and old IPC snapshots decode with `direct_host = None`. This task adds the field and fixes the two in-crate struct literals that would otherwise fail to compile (there are none in this file — the literals live in `handlers/tunnels.rs` tests, fixed in Task 5; but `cargo build -p a2fa-core` must still pass, and it will because the field has a default only for serde, not for struct construction — so we DON'T add construction sites here, only the field).

- [ ] **Step 1: Add the field**

In `auto2fa-rs/crates/a2fa-core/src/model/tunnel.rs`, inside `pub struct Tunnel`, immediately after the `last_user` field (around line 35), add:

```rust
    /// When `Some(host)`, this tunnel forwards local_port → localhost:remote_port
    /// directly ON that registered host (`ssh -N -L … <host>`) — NO jump host and
    /// NO SLURM compute node. `None` = the default SLURM compute-node forward.
    ///
    /// `#[serde(default)]`: old tunnels.json and old IPC snapshots omit this field
    /// and must still decode (→ None = unchanged compute behavior).
    #[serde(default)]
    pub direct_host: Option<String>,
```

- [ ] **Step 2: Build a2fa-core to confirm it compiles**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo build -p a2fa-core`
Expected: builds (a2fa-daemon will NOT build yet — its struct literals in tests/handlers lack the field; that's fixed in later tasks. Do NOT build the daemon in this task.)

- [ ] **Step 3: Add a round-trip serde test proving the default**

Append to the existing `#[cfg(test)] mod tests` block in `tunnel.rs` (create the block at end of file if none exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A tunnel JSON WITHOUT direct_host must decode (→ None) so old
    /// tunnels.json / old snapshots keep working.
    #[test]
    fn direct_host_defaults_to_none_when_absent() {
        let json = r#"{
            "name": "nb", "local_port": 8888, "remote_port": 8888,
            "jump_candidates": null, "last_node": null, "last_user": null,
            "auto_start": false, "post_connect_cmd": null, "tags": [],
            "url_path": null, "status": "idle", "active_jump": null,
            "last_msg": "", "last_alive_at": 0.0, "total_uptime_sec": 0.0,
            "connect_count": 0, "fail_count": 0
        }"#;
        let t: Tunnel = serde_json::from_str(json).expect("decode without direct_host");
        assert_eq!(t.direct_host, None);
    }

    /// A direct tunnel round-trips its host.
    #[test]
    fn direct_host_round_trips() {
        let json = r#"{
            "name": "web", "local_port": 9000, "remote_port": 9000,
            "jump_candidates": null, "last_node": null, "last_user": null,
            "auto_start": false, "post_connect_cmd": null, "tags": [],
            "url_path": null, "direct_host": "loginhost", "status": "idle",
            "active_jump": null, "last_msg": "", "last_alive_at": 0.0,
            "total_uptime_sec": 0.0, "connect_count": 0, "fail_count": 0
        }"#;
        let t: Tunnel = serde_json::from_str(json).expect("decode with direct_host");
        assert_eq!(t.direct_host.as_deref(), Some("loginhost"));
        let back = serde_json::to_value(&t).unwrap();
        assert_eq!(back["direct_host"], "loginhost");
    }
}
```

- [ ] **Step 4: Run the model tests**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-core --lib model::tunnel`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-core/src/model/tunnel.rs
git commit -m "feat(tunnels): add persisted direct_host field to Tunnel model"
```

---

## Task 2: `ForwardSpec` enum + direct argv/spawn in forward.rs

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/tunnels/forward.rs`

Additive only — nothing calls these yet. Keep `build_forward_argv` / `start_forward` (compute) and all their existing tests intact.

- [ ] **Step 1: Write the failing tests**

Append to the existing `#[cfg(test)] mod tests` block in `forward.rs` (after the existing tests, before the closing `}`):

```rust
    // ---- direct mode --------------------------------------------------

    #[test]
    fn direct_argv_has_no_jump_flag() {
        let argv = build_direct_argv("loginhost", 8888, 8888);
        assert!(!argv.contains(&"-J".to_string()), "direct argv must NOT contain -J: {argv:?}");
    }

    #[test]
    fn direct_argv_is_n_dash_l_and_bare_host() {
        let argv = build_direct_argv("loginhost", 7777, 9999);
        assert_eq!(argv[0], "-N");
        assert!(argv.contains(&"7777:localhost:9999".to_string()), "missing forward spec: {argv:?}");
        // The LAST arg is the bare host (no '@', no user).
        assert_eq!(argv.last().unwrap(), "loginhost");
        assert!(!argv.last().unwrap().contains('@'), "direct target must be a bare host");
    }

    #[test]
    fn direct_argv_carries_ssh_opts() {
        let argv = build_direct_argv("h", 1024, 1025);
        assert!(argv.iter().any(|a| a.contains("ExitOnForwardFailure")), "missing ExitOnForwardFailure");
        assert!(argv.iter().any(|a| a.contains("StrictHostKeyChecking=no")), "missing StrictHostKeyChecking=no");
    }

    #[test]
    fn start_forward_direct_rejects_leading_dash_host() {
        assert!(start_forward_direct("-oProxyCommand=x", 1, 2).is_err());
    }

    #[test]
    fn forward_spec_label_returns_jump_or_host() {
        let c = ForwardSpec::Compute { jump: "k6".into(), user: "u".into(), node: "n".into() };
        let d = ForwardSpec::Direct { host: "loginhost".into() };
        assert_eq!(c.label(), "k6");
        assert_eq!(d.label(), "loginhost");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-core --lib tunnels::forward 2>&1 | tail -20`
Expected: FAIL — `cannot find function build_direct_argv` / `cannot find type ForwardSpec`.

- [ ] **Step 3: Add the enum + functions**

In `auto2fa-rs/crates/a2fa-core/src/tunnels/forward.rs`, after the `SSH_OPTS` const (around line 13) and before `build_forward_argv`, add:

```rust
/// What a forward connects to.
///
/// * `Compute` — the SLURM two-hop forward: `ssh -N -J <jump> -L … <user>@<node>`.
/// * `Direct`  — straight to a registered host's own localhost:
///   `ssh -N -L … <host>` (no jump, no node), reusing the host's warm master.
#[derive(Debug, Clone)]
pub enum ForwardSpec {
    Compute { jump: String, user: String, node: String },
    Direct { host: String },
}

impl ForwardSpec {
    /// Host label shown in logs / UI / the tunnel's `active_jump` field
    /// (the jump for compute, the host for direct).
    pub fn label(&self) -> &str {
        match self {
            ForwardSpec::Compute { jump, .. } => jump,
            ForwardSpec::Direct { host } => host,
        }
    }
}
```

Then, immediately after `build_forward_argv` (after its closing `}`, ~line 50), add:

```rust
/// Build the argument list for a DIRECT `ssh -N -L …` forward to `host`'s own
/// localhost. No `-J`, no `user@node` — the bare ssh-config alias is the target,
/// so ssh multiplexes over the host's existing ControlMaster (no new 2FA).
///
/// Pure — fully unit-testable. Returns the argument list (excludes `"ssh"`).
pub fn build_direct_argv(host: &str, local_port: u16, remote_port: u16) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    args.push("-N".into());
    args.push("-L".into());
    args.push(format!("{local_port}:localhost:{remote_port}"));
    for (key, val) in SSH_OPTS {
        args.push("-o".into());
        args.push(format!("{key}={val}"));
    }
    args.push(host.to_string());
    args
}
```

Then, immediately after `start_forward` (after its closing `}`, ~line 95), add:

```rust
/// Spawn a DIRECT `ssh -N -L …` forward to `host`'s own localhost.
///
/// Mirrors [`start_forward`]: rejects a leading `-` in `host` (argument
/// injection, e.g. a host named "-oProxyCommand=…"), and discards all child
/// stdio so the long-lived `ssh -N` can never block on a full pipe buffer.
pub fn start_forward_direct(host: &str, local_port: u16, remote_port: u16) -> Result<Child> {
    if host.starts_with('-') {
        return Err(Error::BadParams(format!(
            "invalid host '{host}': must not start with '-'"
        )));
    }
    let argv = build_direct_argv(host, local_port, remote_port);
    Command::new("ssh")
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::Internal(format!("ssh spawn failed: {e}")))
}

/// Dispatch a forward start by mode. The tunnel workers call only this.
pub fn start_forward_spec(
    spec: &ForwardSpec,
    local_port: u16,
    remote_port: u16,
) -> Result<Child> {
    match spec {
        ForwardSpec::Compute { jump, user, node } => {
            start_forward(jump, user, node, local_port, remote_port)
        }
        ForwardSpec::Direct { host } => start_forward_direct(host, local_port, remote_port),
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-core --lib tunnels::forward 2>&1 | tail -20`
Expected: all forward tests pass (the original ~12 + the 5 new).

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-core/src/tunnels/forward.rs
git commit -m "feat(tunnels): ForwardSpec enum + direct-mode argv/spawn (build_direct_argv, start_forward_direct)"
```

---

## Task 3: Gate the decision functions on `is_direct`

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/tunnel_runtime.rs`

This changes the signatures of `tunnel_action` and `should_autostart`, so **all call sites and existing tests must be updated in this same task** or the crate won't compile. Call sites: `tunnel_maintenance.rs` (`process_tunnel` calls `tunnel_action`; `run_boot_autostart` calls `should_autostart`). They will be threaded in Task 4's lock-step — but since this task changes the signatures, we update those two call sites here too (minimal: pass `false` / the real flag), and the existing tests in `tunnel_runtime.rs`.

- [ ] **Step 1: Write the failing tests**

In `auto2fa-rs/crates/a2fa-daemon/src/tunnel_runtime.rs`, append to the `#[cfg(test)] mod tests` block:

```rust
    // ---- direct-mode gating --------------------------------------------

    #[test]
    fn direct_down_wants_alive_squeue_due_gives_recover_not_squeue() {
        // A DIRECT tunnel has no SLURM job — even with squeue "due" it must
        // go straight to recovery, never SqueueCheck.
        let action = tunnel_action(
            TunnelStatusKind::Failed,
            /*wants_alive=*/ true,
            None,
            /*port_bound=*/ false,
            None,
            /*last_recovery_ts=*/ OLD,
            /*last_squeue_ts=*/ OLD, // would be "due" for a compute tunnel
            NOW,
            /*is_direct=*/ true,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    #[test]
    fn direct_alive_squeue_due_gives_skip_not_squeue() {
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            Some(true),
            true,
            Some(true),
            OLD,
            /*last_squeue_ts=*/ OLD, // due
            NOW,
            /*is_direct=*/ true,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    #[test]
    fn direct_alive_child_dead_still_stop_dead() {
        // Direct tunnels still get the child-died health check.
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            Some(false),
            true,
            Some(true),
            OLD,
            OLD,
            NOW,
            /*is_direct=*/ true,
        );
        assert_eq!(action, TunnelAction::StopDead);
    }

    #[test]
    fn direct_autostart_without_node_is_eligible() {
        // No last_node (direct tunnels never have one), but is_direct → eligible.
        assert!(should_autostart(false, true, None, /*is_direct=*/ true));
        assert!(should_autostart(true, false, None, /*is_direct=*/ true));
        // Not direct + no node → still NOT eligible (unchanged).
        assert!(!should_autostart(false, true, None, /*is_direct=*/ false));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-daemon --lib tunnel_runtime 2>&1 | tail -20`
Expected: FAIL to compile — `tunnel_action` / `should_autostart` take wrong number of args.

- [ ] **Step 3: Add the `is_direct` param to `tunnel_action`**

In `tunnel_runtime.rs`, change the `tunnel_action` signature (around line 463) to add a final param, and gate the two `SqueueCheck` branches. Replace the whole function body's two squeue branches:

Signature — add `is_direct: bool,` as the last parameter:

```rust
#[allow(clippy::too_many_arguments)]
pub fn tunnel_action(
    status: TunnelStatusKind,
    wants_alive: bool,
    child_alive: Option<bool>,
    port_bound: bool,
    jump_active: Option<bool>,
    last_recovery_ts: f64,
    last_squeue_ts: f64,
    now: f64,
    is_direct: bool,
) -> TunnelAction {
```

In the `Idle | Failed | PortBusy if wants_alive` arm, change the squeue condition so direct tunnels skip it:

```rust
        Idle | Failed | PortBusy if wants_alive => {
            if !is_direct && now - last_squeue_ts >= SQUEUE_INTERVAL_SEC {
                TunnelAction::SqueueCheck
            } else if now - last_recovery_ts >= AUTO_RECOVERY_INTERVAL_SEC {
                TunnelAction::Recover
            } else {
                TunnelAction::Skip
            }
        }
```

In the `Alive` arm, change the squeue-due check (Case 3) so direct tunnels skip it:

```rust
            // Case 3: squeue check due (compute tunnels only — direct have no job).
            if !is_direct && now - last_squeue_ts >= SQUEUE_INTERVAL_SEC {
                return TunnelAction::SqueueCheck;
            }

            TunnelAction::Skip
```

- [ ] **Step 4: Add the `is_direct` param to `should_autostart`**

Replace `should_autostart` (around line 535):

```rust
/// Whether a tunnel should be auto-started at boot.
///
/// Compute tunnels need a `last_node` (Python parity). Direct tunnels have no
/// node, so `is_direct` makes them eligible on the want flag alone.
pub fn should_autostart(
    auto_start: bool,
    wants_alive: bool,
    last_node: Option<&str>,
    is_direct: bool,
) -> bool {
    (auto_start || wants_alive) && (last_node.is_some() || is_direct)
}
```

- [ ] **Step 5: Update the existing `tunnel_runtime.rs` tests**

Every existing call to `tunnel_action(...)` in this file's test module needs a trailing `false,` arg, and every existing `should_autostart(a, b, c)` needs a trailing `, false`. Update them:

For `tunnel_action` calls, add `/*is_direct=*/ false,` as the last argument to each of these existing tests:
`wants_alive_idle_recovery_due_squeue_not_due_gives_recover`, `wants_alive_idle_squeue_due_gives_squeue_check`, `wants_alive_stale_gives_skip_not_recover`, `wants_alive_port_busy_squeue_not_due_recovery_due_gives_recover`, `wants_alive_failed_squeue_due_gives_squeue_check`, `wants_alive_failed_squeue_not_due_recovery_due_gives_recover`, `wants_alive_failed_both_throttled_gives_skip`, `wants_alive_idle_throttle_not_elapsed_gives_skip`, `no_wants_alive_idle_gives_skip`, `starting_always_skip`, `alive_child_dead_gives_stop_dead`, `alive_port_not_bound_ghost_gives_stop_dead`, `alive_jump_inactive_gives_stop_disabled_jump`, `alive_jump_active_and_healthy_and_squeue_due_gives_squeue_check`, `alive_all_healthy_squeue_not_due_gives_skip`.

For `should_autostart` calls, add `, false` to each existing test:
`autostart_flag_with_node_gives_true`, `wants_alive_with_node_gives_true`, `autostart_flag_without_node_gives_false`, `wants_alive_without_node_gives_false`, `neither_flag_set_gives_false`.

(These are the SLURM-mode tests, so `is_direct = false` preserves their meaning exactly.)

- [ ] **Step 6: Update the two production call sites**

In `auto2fa-rs/crates/a2fa-daemon/src/tunnel_maintenance.rs`:

(a) `process_tunnel` calls `tunnel_action(...)` (around line 205). Add the direct flag as the final arg:

```rust
    let action = tunnel_action(
        status_kind,
        snap.wants_alive,
        child_alive,
        port_bound,
        snap.jump_host_active,
        last_recovery_ts,
        last_squeue_ts,
        now,
        snap.direct_host.is_some(),
    );
```

(NOTE: `snap.direct_host` does not exist yet — it is added to `TunnelSnapshot` in Task 5. To keep THIS task compiling, temporarily pass `false` here and change it to `snap.direct_host.is_some()` in Task 5. Use `false` for now.)

So for THIS task, write:

```rust
        now,
        false, // is_direct — wired to snap.direct_host.is_some() in Task 5
    );
```

(b) `run_boot_autostart` filters with `should_autostart(...)` (around line 318). For THIS task, add `, false`:

```rust
            .filter(|t| should_autostart(t.auto_start, t.wants_alive, t.last_node.as_deref(), false))
```

(NOTE: changed to `t.direct_host.is_some()` in Task 5.)

- [ ] **Step 7: Run the runtime tests + build the daemon lib**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-daemon --lib tunnel_runtime 2>&1 | tail -20`
Expected: all tunnel_runtime tests pass (existing + 4 new). The crate compiles.

- [ ] **Step 8: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-daemon/src/tunnel_runtime.rs auto2fa-rs/crates/a2fa-daemon/src/tunnel_maintenance.rs
git commit -m "feat(tunnels): is_direct gate on tunnel_action + should_autostart (suppress squeue/node-gate for direct)"
```

---

## Task 4: Workers take `ForwardSpec`

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/workers.rs`
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/handlers/tunnels.rs` (the two `spawn_tunnel_start*` call sites in `tunnel_start`)
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/tunnel_maintenance.rs` (its private `spawn_tunnel_start_with_runtime` + `do_tunnel_start` call site)

This replaces the `(jump, user, node)` triple with `spec: ForwardSpec` across the workers. Because the daemon's `tunnel_start` handler still resolves jump/user/node the old way, **this task keeps that resolution but wraps the result into `ForwardSpec::Compute` at the call site** — the direct branch is added in Task 5. So after this task, behavior is identical (all compute), only the worker plumbing changed.

- [ ] **Step 1: Change `workers.rs` worker signatures to `ForwardSpec`**

In `auto2fa-rs/crates/a2fa-daemon/src/workers.rs`:

Replace the public `spawn_tunnel_start` (lines ~283-306) and `spawn_tunnel_start_with_runtime` (lines ~312-336) and the private `spawn_tunnel_start_inner` (lines ~339+) so they take `spec: ForwardSpec` instead of `jump: String, user: String, node: String`. Full replacements:

```rust
pub fn spawn_tunnel_start(
    name: String,
    spec: a2fa_core::tunnels::forward::ForwardSpec,
    local_port: u16,
    remote_port: u16,
    post_connect_cmd: Option<String>,
    state: Arc<Mutex<State>>,
    post_connect_running: Arc<Mutex<std::collections::HashSet<String>>>,
) {
    spawn_tunnel_start_inner(
        name, spec, local_port, remote_port, post_connect_cmd, state,
        post_connect_running, None,
    );
}

pub fn spawn_tunnel_start_with_runtime(
    name: String,
    spec: a2fa_core::tunnels::forward::ForwardSpec,
    local_port: u16,
    remote_port: u16,
    post_connect_cmd: Option<String>,
    state: Arc<Mutex<State>>,
    post_connect_running: Arc<Mutex<std::collections::HashSet<String>>>,
    runtime: Arc<crate::tunnel_runtime::TunnelRuntime>,
) {
    spawn_tunnel_start_inner(
        name, spec, local_port, remote_port, post_connect_cmd, state,
        post_connect_running, Some(runtime),
    );
}
```

Then rewrite `spawn_tunnel_start_inner` to take `spec` and branch internally. Replace its signature and the body up to and including the connect-record. The signature:

```rust
#[allow(clippy::too_many_arguments)]
fn spawn_tunnel_start_inner(
    name: String,
    spec: a2fa_core::tunnels::forward::ForwardSpec,
    local_port: u16,
    remote_port: u16,
    post_connect_cmd: Option<String>,
    state: Arc<Mutex<State>>,
    post_connect_running: Arc<Mutex<std::collections::HashSet<String>>>,
    runtime: Option<Arc<crate::tunnel_runtime::TunnelRuntime>>,
) {
```

Inside the spawned thread body, replace the `use` line and the `start_forward(...)` call and every later use of `jump` / `node`. Concretely:

Change the imports line:
```rust
            use a2fa_core::tunnels::forward::{probe_and_settle, start_forward_spec, ForwardSpec, ProbeOutcome};
```

Add, right after the imports, label + post-connect target derivation:
```rust
            let label = spec.label().to_string();
            // Post-connect needs a (node, jump) pair; for direct, host stands in for both.
            let (pc_node, pc_jump) = match &spec {
                ForwardSpec::Compute { node, jump, .. } => (node.clone(), jump.clone()),
                ForwardSpec::Direct { host } => (host.clone(), host.clone()),
            };
            // Human target for the connect record.
            let target = match &spec {
                ForwardSpec::Compute { node, .. } => format!("{node}:{remote_port}"),
                ForwardSpec::Direct { host } => format!("{host}:{remote_port} (direct)"),
            };

            info!("[tunnel:{name}] starting via {label}");
```

Replace `let child = match start_forward(&jump, &user, &node, local_port, remote_port) {` with:
```rust
            let child = match start_forward_spec(&spec, local_port, remote_port) {
```

In the `ProbeOutcome::Ready` arm, replace the connect record line:
```rust
                        rt.record(&name, now, format!("connected via {label} → {target}"));
```
and the `t.active_jump = Some(jump.clone());` line with:
```rust
                        t.active_jump = Some(label.clone());
```
and the `t.last_msg = format!("via {jump}");` line with:
```rust
                        t.last_msg = format!("via {label}");
```
and `info!("[tunnel:{name}] alive via {jump}");` with:
```rust
                        info!("[tunnel:{name}] alive via {label}");
```

In the post-connect call inside that arm, replace `node.clone(), jump.clone(),` with `pc_node.clone(), pc_jump.clone(),`:
```rust
                        run_post_connect(
                            name.clone(),
                            cmd,
                            local_port,
                            pc_node.clone(),
                            pc_jump.clone(),
                            post_connect_running,
                        );
```

The failure/timeout/error arms reference only `local_port` and `name` (not `jump`/`node`) — leave them unchanged.

- [ ] **Step 2: Update the IPC handler call sites in `handlers/tunnels.rs`**

In `auto2fa-rs/crates/a2fa-daemon/src/handlers/tunnels.rs`, `tunnel_start` ends by building a `(jump, user, node, local_port, remote_port, post_connect_cmd)` tuple and matching on `runtime`. Wrap jump/user/node into a `ForwardSpec::Compute` at the call sites. Replace the final `match runtime { ... }` block (lines ~351-375):

```rust
    let spec = a2fa_core::tunnels::forward::ForwardSpec::Compute { jump, user, node };

    match runtime {
        Some(rt) => spawn_tunnel_start_with_runtime(
            name,
            spec,
            local_port,
            remote_port,
            post_connect_cmd,
            Arc::clone(state),
            post_connect_running,
            rt,
        ),
        None => spawn_tunnel_start(
            name,
            spec,
            local_port,
            remote_port,
            post_connect_cmd,
            Arc::clone(state),
            post_connect_running,
        ),
    }
```

- [ ] **Step 3: Update the maintenance worker + its call site**

In `auto2fa-rs/crates/a2fa-daemon/src/tunnel_maintenance.rs`, the PRIVATE `spawn_tunnel_start_with_runtime` (lines ~741-903) also takes `(jump, user, node)`. Apply the same transformation:

Signature — replace `jump: String, user: String, node: String,` and drop the unused `_snap_local_port: u16,` param's siblings carefully. New signature:

```rust
#[allow(clippy::too_many_arguments)]
fn spawn_tunnel_start_with_runtime(
    name: String,
    spec: a2fa_core::tunnels::forward::ForwardSpec,
    local_port: u16,
    remote_port: u16,
    _snap_local_port: u16,
    post_connect_cmd: Option<String>,
    state: Arc<Mutex<State>>,
    post_connect_running: Arc<Mutex<HashSet<String>>>,
    runtime: Arc<TunnelRuntime>,
) {
```

Inside the thread body, change the imports line:
```rust
            use a2fa_core::tunnels::forward::{probe_and_settle, start_forward_spec, ForwardSpec, ProbeOutcome};
```

Add after the imports:
```rust
            let label = spec.label().to_string();
            let (pc_node, pc_jump) = match &spec {
                ForwardSpec::Compute { node, jump, .. } => (node.clone(), jump.clone()),
                ForwardSpec::Direct { host } => (host.clone(), host.clone()),
            };
            let target = match &spec {
                ForwardSpec::Compute { node, .. } => format!("{node}:{remote_port}"),
                ForwardSpec::Direct { host } => format!("{host}:{remote_port} (direct)"),
            };

            info!("[tunnel:{name}] maintenance: starting via {label}");
```

Replace `let child = match start_forward(&jump, &user, &node, local_port, remote_port) {` with:
```rust
            let child = match start_forward_spec(&spec, local_port, remote_port) {
```

In the `Ready` arm: replace `runtime.record(&name, now, format!("connected via {jump} → {node}:{remote_port}"));` with:
```rust
                    runtime.record(&name, now, format!("connected via {label} → {target}"));
```
replace `t.active_jump = Some(jump.clone());` with `t.active_jump = Some(label.clone());`
replace `t.last_msg = format!("via {jump}");` with `t.last_msg = format!("via {label}");`
replace the post-connect args `node.clone(), jump.clone(),` with `pc_node.clone(), pc_jump.clone(),`.

The other arms use only `local_port`/`name` — leave unchanged.

Then update `do_tunnel_start`'s call to this function (lines ~715-727). It currently passes `jump, user, node`. Wrap into `ForwardSpec::Compute`:

```rust
    if let Some((jump, user, node, local_port, remote_port, post_cmd)) = start_info {
        let spec = a2fa_core::tunnels::forward::ForwardSpec::Compute { jump, user, node };
        spawn_tunnel_start_with_runtime(
            name.to_owned(),
            spec,
            local_port,
            remote_port,
            snap.local_port,
            post_cmd,
            Arc::clone(state),
            Arc::clone(post_connect_running),
            Arc::clone(runtime),
        );
    }
```

- [ ] **Step 4: Build the whole daemon + run the existing tunnel tests**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo build -p a2fa-daemon && cargo test -p a2fa-daemon --lib handlers::tunnels 2>&1 | tail -20`
Expected: daemon builds; existing `handlers::tunnels` tests pass unchanged (behavior is still all-compute).

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-daemon/src/workers.rs auto2fa-rs/crates/a2fa-daemon/src/handlers/tunnels.rs auto2fa-rs/crates/a2fa-daemon/src/tunnel_maintenance.rs
git commit -m "refactor(tunnels): thread ForwardSpec through both start-workers (no behavior change)"
```

---

## Task 5: Daemon direct branch — `tunnel_add`, snapshot, `tunnel_start`, `do_tunnel_start`, `TunnelSnapshot`

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/handlers/tunnels.rs`
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/tunnel_maintenance.rs`

Wires the actual direct behavior: create a direct tunnel, snapshot the field, and branch both start paths on `direct_host`.

- [ ] **Step 1: Write the failing tests**

In `auto2fa-rs/crates/a2fa-daemon/src/handlers/tunnels.rs`, append to the `#[cfg(test)] mod tests` block. First, note the existing test helpers (`make_state`, `make_state_with_tunnel`) build `Tunnel` struct literals — those literals will fail to compile once the field is added unless they set it. So **Step 3 also fixes the helpers**. The new tests:

```rust
    // ---- direct mode ---------------------------------------------------

    #[test]
    fn tunnel_add_direct_host_stored_and_in_snapshot() {
        let state = make_state();
        let snap = tunnel_add(
            &state,
            &json!({"name": "web", "local_port": 9000, "direct_host": "loginhost"}),
        )
        .unwrap();
        assert_eq!(snap["direct_host"], "loginhost");
        let guard = crate::lock_state(&state);
        assert_eq!(guard.tunnels[0].direct_host.as_deref(), Some("loginhost"));
    }

    #[test]
    fn tunnel_add_without_direct_host_is_none() {
        let state = make_state();
        let snap = tunnel_add(&state, &json!({"name": "nb", "local_port": 8888})).unwrap();
        assert!(snap["direct_host"].is_null());
        assert_eq!(crate::lock_state(&state).tunnels[0].direct_host, None);
    }

    #[test]
    fn tunnel_add_direct_host_leading_dash_rejected() {
        let state = make_state();
        let err = tunnel_add(
            &state,
            &json!({"name": "x", "local_port": 9001, "direct_host": "-oProxyCommand=x"}),
        )
        .unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    /// A direct tunnel whose host is not registered/ready must NOT spawn — it
    /// parks Idle with a "waiting for host" message (maintenance recovers it).
    #[test]
    fn tunnel_start_direct_no_ready_host_waits() {
        let state = make_state();
        tunnel_add(
            &state,
            &json!({"name": "web", "local_port": 9002, "direct_host": "loginhost"}),
        )
        .unwrap();
        tunnel_start(&state, &json!({"name": "web"}), None, None).unwrap();
        let guard = crate::lock_state(&state);
        let t = &guard.tunnels[0];
        assert_eq!(t.status, TunnelStatus::Idle);
        assert!(t.last_msg.contains("waiting for host"), "got: {}", t.last_msg);
        assert_eq!(t.active_jump.as_deref(), Some("loginhost"));
        assert!(t.wants_alive, "wants_alive must be set so maintenance retries");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-daemon --lib handlers::tunnels 2>&1 | tail -25`
Expected: FAIL to compile — `Tunnel` literals in test helpers + `tunnel_add` body missing `direct_host`.

- [ ] **Step 3: Add `direct_host` to every `Tunnel` struct literal in this file**

In `handlers/tunnels.rs`, the `tunnel_add` function builds a `Tunnel { … }` (around line 166). Add the field. AND every test helper / inline literal (`make_state_with_tunnel`, `make_alive_tunnel`, `make_tunnel_with_status`, and the `tunnel_rename_duplicate_returns_error` inline loop) builds a `Tunnel { … }` — add `direct_host: None,` to each.

For the production `tunnel_add` literal, parse the param first. Before the `let mut guard = crate::lock_state(state);` line in `tunnel_add` (around line 149), add:

```rust
    // Optional direct-mode target: forward straight to this registered host's
    // own localhost (no jump / no node). Reject a leading '-' (ssh arg injection).
    let direct_host: Option<String> = match params.get("direct_host") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let s = v.as_str().unwrap_or("").trim().to_owned();
            if s.is_empty() {
                None
            } else if s.starts_with('-') {
                return Err(Error::BadParams(format!(
                    "invalid direct_host '{s}': must not start with '-'"
                )));
            } else {
                Some(s)
            }
        }
    };
```

Then in the `Tunnel { … }` literal, add `direct_host,` (shorthand) after `last_user: None,`:

```rust
        last_user: None,
        direct_host,
        auto_start: false,
```

For each TEST helper literal, add `direct_host: None,` after the `last_user: …,` line. The helpers are: `make_state_with_tunnel` (~line 1041), `make_alive_tunnel` (~line 1064), `make_tunnel_with_status` (~line 1088), and the loop in `tunnel_rename_duplicate_returns_error` (~line 1412).

- [ ] **Step 4: Add `direct_host` to `tunnel_snapshot`**

In `handlers/tunnels.rs`, `tunnel_snapshot` (around line 68) — add the field to the JSON, after `"last_user": t.last_user,`:

```rust
        "last_user": t.last_user,
        "direct_host": t.direct_host,
        "auto_start": t.auto_start,
```

- [ ] **Step 5: Add the direct branch to `tunnel_start`**

In `handlers/tunnels.rs`, `tunnel_start` — the locked block currently resolves jump/user/node and returns a tuple, then the caller wraps into `ForwardSpec::Compute`. Restructure so the locked block returns a `ForwardSpec` directly, branching on `direct_host`.

Replace the entire `let (jump, user, node, local_port, remote_port, post_connect_cmd) = { … };` block (lines ~264-341) with a block that yields `Option<(ForwardSpec, u16, u16, Option<String>)>` (None = parked, already wrote status):

```rust
    let resolved: Option<(a2fa_core::tunnels::forward::ForwardSpec, u16, u16, Option<String>)> = {
        let mut guard = crate::lock_state(state);
        let t = guard
            .tunnels
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.clone()))?;

        // Idempotent + in-flight latch.
        if matches!(t.status, TunnelStatus::Alive | TunnelStatus::Starting) {
            return Ok(Value::Null);
        }

        let direct_host = t.direct_host.clone();
        let local_port = t.local_port;
        let remote_port = t.remote_port;
        let post_cmd = t.post_connect_cmd.clone();

        match direct_host {
            // ---- Direct mode: forward to <host>'s own localhost ----
            Some(host) => {
                let ready = guard
                    .hosts
                    .iter()
                    .any(|h| h.host == host && h.is_master_ready);
                let t = guard.tunnels.iter_mut().find(|t| t.name == name).unwrap();
                if !ready {
                    // Park until the host's master is up; maintenance recovers it.
                    t.status = TunnelStatus::Idle;
                    t.last_msg = format!("waiting for host {host}");
                    t.active_jump = Some(host.clone());
                    t.wants_alive = true;
                    return Ok(Value::Null);
                }
                t.status = TunnelStatus::Starting;
                t.active_jump = Some(host.clone());
                t.last_msg = format!("starting direct to {host}");
                t.wants_alive = true;
                Some((
                    a2fa_core::tunnels::forward::ForwardSpec::Direct { host },
                    local_port,
                    remote_port,
                    post_cmd,
                ))
            }
            // ---- Compute mode: SLURM two-hop (unchanged) ----
            None => {
                let jump = guard
                    .hosts
                    .iter()
                    .find(|h| h.is_master_ready && {
                        let t = guard.tunnels.iter().find(|t| t.name == name).unwrap();
                        match &t.jump_candidates {
                            Some(cands) => cands.contains(&h.host),
                            None => true,
                        }
                    })
                    .map(|h| h.host.clone());

                let t = guard.tunnels.iter_mut().find(|t| t.name == name).unwrap();

                let node = match t.last_node.clone() {
                    Some(n) => n,
                    None => {
                        t.status = TunnelStatus::Idle;
                        t.last_msg = "no node — press Enter to pick".into();
                        return Ok(Value::Null);
                    }
                };
                let jump = match jump {
                    Some(j) => j,
                    None => {
                        t.status = TunnelStatus::Idle;
                        t.last_msg = "waiting for jump host".into();
                        return Ok(Value::Null);
                    }
                };
                let user = t
                    .last_user
                    .clone()
                    .unwrap_or_else(|| std::env::var("USER").unwrap_or_default());
                if user.is_empty() {
                    t.status = TunnelStatus::Failed;
                    t.last_msg = "no user (set last_user in tunnels.json)".into();
                    return Ok(Value::Null);
                }
                t.status = TunnelStatus::Starting;
                t.active_jump = Some(jump.clone());
                t.last_msg = format!("starting via {jump}");
                t.wants_alive = true;
                Some((
                    a2fa_core::tunnels::forward::ForwardSpec::Compute { jump, user, node },
                    local_port,
                    remote_port,
                    post_cmd,
                ))
            }
        }
    };

    let (spec, local_port, remote_port, post_connect_cmd) = match resolved {
        Some(v) => v,
        None => return Ok(Value::Null),
    };
```

Then make ONE deletion in the dispatch area. Task 4 inserted a line
`let spec = a2fa_core::tunnels::forward::ForwardSpec::Compute { jump, user, node };`
just above the `match runtime { … }`. That line is now obsolete and broken (the
variables `jump`/`user`/`node` no longer exist at this scope, and `spec` is
already bound by the `let (spec, local_port, remote_port, post_connect_cmd) =
match resolved { … };` destructure above). **Delete only that one line.**

Do NOT touch the existing `let post_connect_running = post_connect_running.unwrap_or_else(…);`
line or the `match runtime { … }` dispatch — they already reference `spec`,
`local_port`, `remote_port`, and `post_connect_cmd` correctly. (Re-declaring
`post_connect_running` here would shadow the `Option` with an `Arc` and fail to
compile on the second `.unwrap_or_else`.) After the deletion, the tail of
`tunnel_start` reads:

```rust
    let (spec, local_port, remote_port, post_connect_cmd) = match resolved {
        Some(v) => v,
        None => return Ok(Value::Null),
    };

    // (existing comment block about the SHARED post-connect dedup set)
    let post_connect_running: Arc<Mutex<HashSet<String>>> =
        post_connect_running.unwrap_or_else(|| Arc::new(Mutex::new(HashSet::new())));

    match runtime {
        Some(rt) => spawn_tunnel_start_with_runtime(
            name, spec, local_port, remote_port, post_connect_cmd,
            Arc::clone(state), post_connect_running, rt,
        ),
        None => spawn_tunnel_start(
            name, spec, local_port, remote_port, post_connect_cmd,
            Arc::clone(state), post_connect_running,
        ),
    }

    Ok(Value::Null)
```

- [ ] **Step 6: Add the direct branch to `do_tunnel_start` (maintenance) + `TunnelSnapshot.direct_host`**

In `auto2fa-rs/crates/a2fa-daemon/src/tunnel_maintenance.rs`:

(a) Add `direct_host: Option<String>` to the `TunnelSnapshot` struct (around line 162) after `last_user`:

```rust
    last_user: Option<String>,
    direct_host: Option<String>,
```

(b) Populate it in BOTH places `TunnelSnapshot { … }` is built — in `maintenance_tick` (~line 132) and in `run_boot_autostart` (~line 319). After each `last_user: t.last_user.clone(),` add:

```rust
                last_user: t.last_user.clone(),
                direct_host: t.direct_host.clone(),
```

(c) Now wire the two deferred Task-3 placeholders to real values:
- In `process_tunnel`, change the `false, // is_direct …` line in the `tunnel_action` call to:
```rust
        snap.direct_host.is_some(),
```
- In `run_boot_autostart`, change the filter to:
```rust
            .filter(|t| should_autostart(t.auto_start, t.wants_alive, t.last_node.as_deref(), t.direct_host.is_some()))
```

(d) Add the direct branch to `do_tunnel_start` (around line 631). The function currently resolves jump/user/node under the lock into `start_info: Option<(String,String,String,u16,u16,Option<String>)>`, then (per Task 4) wraps that into `ForwardSpec::Compute` at the call site. Restructure so `start_info` yields a `ForwardSpec` **directly**, branching on `direct_host`. Replace the **whole** `let start_info: Option<(…)> = { … };` block (lines ~639-708) with the block below (this supersedes Task 4's `(jump,user,node,…)` tuple shape entirely):

```rust
    let start_info: Option<(a2fa_core::tunnels::forward::ForwardSpec, u16, u16, Option<String>)> = {
        let mut guard = crate::lock_state(state);

        let t = match guard.tunnels.iter().find(|t| t.name == name) {
            Some(t) => t,
            None => return,
        };
        if !t.wants_alive {
            info!("[tunnel:{name}] do_tunnel_start: wants_alive cleared by user — skipping");
            return;
        }
        if matches!(t.status, TunnelStatus::Alive | TunnelStatus::Starting) {
            return;
        }

        let direct_host = t.direct_host.clone();
        let local_port = t.local_port;
        let remote_port = t.remote_port;
        let post_cmd = t.post_connect_cmd.clone();

        match direct_host {
            Some(host) => {
                let ready = guard.hosts.iter().any(|h| h.host == host && h.is_master_ready);
                let t = guard.tunnels.iter_mut().find(|t| t.name == name).unwrap();
                if !ready {
                    t.status = TunnelStatus::Idle;
                    t.last_msg = format!("waiting for host {host}");
                    t.active_jump = Some(host.clone());
                    return;
                }
                t.status = TunnelStatus::Starting;
                t.active_jump = Some(host.clone());
                t.last_msg = format!("starting direct to {host}");
                Some((
                    a2fa_core::tunnels::forward::ForwardSpec::Direct { host },
                    local_port,
                    remote_port,
                    post_cmd,
                ))
            }
            None => {
                let jump = {
                    let candidates = t.jump_candidates.clone();
                    guard.hosts.iter().find(|h| {
                        h.is_master_ready && match &candidates {
                            Some(cs) => cs.contains(&h.host),
                            None => true,
                        }
                    }).map(|h| h.host.clone())
                };
                let t = guard.tunnels.iter_mut().find(|t| t.name == name).unwrap();
                let node = match t.last_node.clone() {
                    Some(n) => n,
                    None => {
                        t.status = TunnelStatus::Idle;
                        t.last_msg = "no node — press Enter to pick".into();
                        return;
                    }
                };
                let jump = match jump {
                    Some(j) => j,
                    None => {
                        t.status = TunnelStatus::Idle;
                        t.last_msg = "waiting for jump host".into();
                        return;
                    }
                };
                let user = t
                    .last_user
                    .clone()
                    .unwrap_or_else(|| std::env::var("USER").unwrap_or_default());
                if user.is_empty() {
                    t.status = TunnelStatus::Failed;
                    t.last_msg = "no user (set last_user in tunnels.json)".into();
                    return;
                }
                t.status = TunnelStatus::Starting;
                t.active_jump = Some(jump.clone());
                t.last_msg = format!("starting via {jump}");
                Some((
                    a2fa_core::tunnels::forward::ForwardSpec::Compute { jump, user, node },
                    local_port,
                    remote_port,
                    post_cmd,
                ))
            }
        }
    };
```

Then replace the dispatch (lines ~710-728) — `spec` is already built:

```rust
    if let Some((spec, local_port, remote_port, post_cmd)) = start_info {
        spawn_tunnel_start_with_runtime(
            name.to_owned(),
            spec,
            local_port,
            remote_port,
            snap.local_port,
            post_cmd,
            Arc::clone(state),
            Arc::clone(post_connect_running),
            Arc::clone(runtime),
        );
    }
```

This replacement block destructures `spec` straight out of `start_info`, so Task 4's intermediate `let spec = ForwardSpec::Compute { jump, user, node };` line is gone (the whole `if let Some((jump, user, node, …)) { … }` block is replaced). Nothing else in `do_tunnel_start` references `jump`/`user`/`node` afterward.

- [ ] **Step 7: Build + run the daemon tests**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo build -p a2fa-daemon && cargo test -p a2fa-daemon --lib 2>&1 | tail -25`
Expected: builds; all daemon tests pass (existing + 4 new direct tests in handlers::tunnels).

- [ ] **Step 8: Run the full Rust suite + clippy**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test 2>&1 | tail -15 && cargo clippy --all-targets 2>&1 | tail -15`
Expected: all tests pass; no NEW clippy errors (pre-existing style warnings, if any, are acceptable — do not introduce new ones).

- [ ] **Step 9: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-daemon/src/handlers/tunnels.rs auto2fa-rs/crates/a2fa-daemon/src/tunnel_maintenance.rs
git commit -m "feat(tunnels): direct-mode daemon paths — tunnel_add/start + maintenance branch on direct_host"
```

---

## Task 6: Swift model + IPC + AppState

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Models/Tunnel.swift`
- Modify: `auto2fa-mac/Auto2FA/BackendClient.swift`
- Modify: `auto2fa-mac/Auto2FA/AppState.swift`

- [ ] **Step 1: Add `directHost` to the Swift Tunnel model**

In `auto2fa-mac/Auto2FA/Models/Tunnel.swift`:

(a) Add the stored property after `lastUser` (line 10):
```swift
    let lastUser: String?
    let directHost: String?
```

(b) Add the coding key in the `CodingKeys` enum after `lastUser`:
```swift
        case lastUser = "last_user"
        case directHost = "direct_host"
```

(c) Decode it in `init(from:)` after the `lastUser` decode (line 61):
```swift
        self.lastUser = try c.decodeIfPresent(String.self, forKey: .lastUser)
        self.directHost = try c.decodeIfPresent(String.self, forKey: .directHost)
```

(d) Add a convenience accessor after `var url` (line 24):
```swift
    /// True when this tunnel forwards straight to a registered host's own
    /// localhost (no jump / no SLURM node).
    var isDirect: Bool { directHost != nil }
```

- [ ] **Step 2: Pass `direct_host` through the IPC client**

In `auto2fa-mac/Auto2FA/BackendClient.swift`, replace `addTunnel` (lines 463-468):

```swift
    func addTunnel(name: String, localPort: Int, remotePort: Int? = nil,
                   directHost: String? = nil) async throws -> Tunnel {
        var params: [String: Any] = ["name": name, "local_port": localPort]
        if let rp = remotePort { params["remote_port"] = rp }
        if let dh = directHost, !dh.isEmpty { params["direct_host"] = dh }
        let data = try await sendRaw(method: "tunnel_add", params: params)
        return try JSONDecoder().decode(Tunnel.self, from: data)
    }
```

- [ ] **Step 3: Pass `directHost` through AppState.createTunnel**

In `auto2fa-mac/Auto2FA/AppState.swift`, replace the `createTunnel` signature + the `addTunnel` call (lines 821-827):

```swift
    func createTunnel(name: String, localPort: Int, remotePort: Int? = nil,
                      autoStart: Bool = false, directHost: String? = nil) async -> String? {
        inFlightTunnels.insert(name)
        defer { inFlightTunnels.remove(name) }
        do {
            _ = try await client.addTunnel(name: name, localPort: localPort,
                                           remotePort: remotePort, directHost: directHost)
            if autoStart {
                try? await client.setTunnelAutostart(name, value: true)
            }
            dismissSheet()
            await reloadAll()
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }
```

- [ ] **Step 4: Build the app to confirm it compiles**

Run:
```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -configuration Debug -destination 'platform=macOS' build 2>&1 | tail -15
```
Expected: `** BUILD SUCCEEDED **`. (If the scheme/project name differs, list with `xcodebuild -list -project Auto2FA.xcodeproj` and use the actual scheme.)

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-mac/Auto2FA/Models/Tunnel.swift auto2fa-mac/Auto2FA/BackendClient.swift auto2fa-mac/Auto2FA/AppState.swift
git commit -m "feat(ui): decode + plumb direct_host through Tunnel model, IPC client, AppState"
```

---

## Task 7: Swift UI — target picker + direct-aware row

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/NewTunnelSheet.swift`
- Modify: `auto2fa-mac/Auto2FA/Views/Components/TunnelRow.swift`

- [ ] **Step 1: Add the target picker to NewTunnelSheet**

In `auto2fa-mac/Auto2FA/Views/NewTunnelSheet.swift`:

(a) Add a target enum + state. After `enum Field { case name, port }` (line 26) add:
```swift
    enum Target: String, CaseIterable, Identifiable {
        case compute = "Compute node (SLURM)"
        case direct = "Direct to a host"
        var id: String { rawValue }
    }
    @State private var target: Target = .compute
    @State private var selectedHost: String = ""
```

(b) Add the picker UI. In `body`, inside the form-fields `VStack` (after the `fieldGroup("Template") { … }` block that ends ~line 84, before `fieldGroup("Name")`), insert:
```swift
                fieldGroup("Target") {
                    Picker("Target", selection: $target) {
                        ForEach(Target.allCases) { t in Text(t.rawValue).tag(t) }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                }

                if target == .direct {
                    fieldGroup("Host") {
                        if appState.hosts.isEmpty {
                            Text("No registered hosts yet — add a host first, then forward a port to it.")
                                .font(.caption).foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        } else {
                            Picker("Host", selection: $selectedHost) {
                                ForEach(appState.hosts, id: \.host) { h in
                                    Text(h.host).tag(h.host)
                                }
                            }
                            .labelsHidden()
                        }
                    }
                }
```

(c) Default-select the first host. In the `.task { … }` block (after `applyTemplate(template)`, ~line 158), add:
```swift
            if selectedHost.isEmpty, let first = appState.hosts.first { selectedHost = first.host }
```

(d) Wire submit. In `submit()` (lines 237-262), after the port guard and before `submitting = true`, add a direct-host guard; then pass `directHost` to `createTunnel`. Replace the body from `submitting = true` to the end of `submit()`:
```swift
        var directHost: String? = nil
        if target == .direct {
            let h = selectedHost.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !h.isEmpty else {
                error = appState.hosts.isEmpty
                    ? "Add a host first, then forward a port to it."
                    : "Pick a host to forward to."
                return
            }
            directHost = h
        }
        submitting = true
        error = nil
        Task {
            if let errMsg = await appState.createTunnel(name: trimmedName,
                                                       localPort: port,
                                                       remotePort: parsedRemotePort,
                                                       autoStart: autoStart,
                                                       directHost: directHost) {
                error = errMsg
                submitting = false
            }
        }
    }
```

- [ ] **Step 2: Make TunnelRow direct-aware**

In `auto2fa-mac/Auto2FA/Views/Components/TunnelRow.swift`:

(a) The node column (lines 128-154): replace the inner node `Group { … }` + `viaMenu` with direct-aware variants. Replace the block:
```swift
            if !hovering && !isFailedState {
                // Node (secondary; "(no node)" tertiary) — flexible column.
                Group {
                    if let n = tunnel.lastNode {
                        Text(n)
                            .font(.rowIdentifier)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    } else {
                        Text("(no node)")
                            .font(.rowMeta)
                            .foregroundStyle(.tertiary)
                            .italic()
                            .lineLimit(1)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                // via <jump> — clickable jump-host Menu (also in the ⋯ overflow).
                viaMenu
                    .frame(width: 70, alignment: .leading)

                // Metadata: aliveSince + fail count — compact fixed column.
                metadata
                    .frame(width: 92, alignment: .leading)
            }
```
with:
```swift
            if !hovering && !isFailedState {
                // Target column: direct → "→ host"; compute → node or "(no node)".
                Group {
                    if tunnel.isDirect {
                        Text("→ \(tunnel.directHost ?? "host")")
                            .font(.rowIdentifier)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    } else if let n = tunnel.lastNode {
                        Text(n)
                            .font(.rowIdentifier)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    } else {
                        Text("(no node)")
                            .font(.rowMeta)
                            .foregroundStyle(.tertiary)
                            .italic()
                            .lineLimit(1)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                // direct → static "direct" label; compute → clickable jump menu.
                Group {
                    if tunnel.isDirect {
                        Text("direct")
                            .font(.rowMeta)
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                    } else {
                        viaMenu
                    }
                }
                .frame(width: 70, alignment: .leading)

                // Metadata: aliveSince + fail count — compact fixed column.
                metadata
                    .frame(width: 92, alignment: .leading)
            }
```

(b) Hide the failed-state "Node" recovery button for direct (lines 176-183). Wrap the existing `if tunnel.displayState != .portBusy { … Node … }` with an extra `!tunnel.isDirect` condition:
```swift
                if tunnel.displayState != .portBusy && !tunnel.isDirect {
                    Button { appState.presentNodePicker(for: tunnel) } label: {
                        Label("Node", systemImage: "list.bullet.rectangle")
                    }
                    .buttonStyle(.glass).controlSize(.small)
                    .disabled(appState.inFlightTunnels.contains(tunnel.name))
                    .transition(.opacity)
                }
```

(c) Hide the hover-bar "Node" button for direct (lines 286-292). Wrap it:
```swift
                if !tunnel.isDirect {
                    glassActionButton(id: "node",
                                      disabled: isBusy,
                                      help: "Pick compute node") {
                        appState.presentNodePicker(for: tunnel)
                    } label: {
                        Label("Node", systemImage: "list.bullet.rectangle")
                    }
                }
```

(d) Hide the overflow "Pick node…" item + "Use jump host" submenu for direct (lines 368-379). Wrap both:
```swift
        if !tunnel.isDirect {
            Button {
                appState.presentNodePicker(for: tunnel)
            } label: {
                Label("Pick node…", systemImage: "list.bullet.rectangle")
            }
            .disabled(isBusy)

            Menu {
                jumpPickerMenu(for: tunnel)
            } label: {
                Label("Use jump host", systemImage: "arrow.triangle.branch")
            }
        }
```

- [ ] **Step 3: Build the app**

Run:
```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -configuration Debug -destination 'platform=macOS' build 2>&1 | tail -15
```
Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 4: Manual QA checklist (run the built app)**

Verify by inspection / running the Debug build:
- New Tunnel sheet shows the **Target** picker; "Direct to a host" reveals a host dropdown; with no hosts it shows the "add a host first" note.
- Creating a **compute** tunnel still works exactly as before (node picker, via menu, countdown).
- Creating a **direct** tunnel: the row shows `→ host` + `direct`, NO node/jump/countdown affordances, and (with a connected host) Start brings it up instantly with no 2FA; Open-in-browser / Copy / Stop work.

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-mac/Auto2FA/Views/NewTunnelSheet.swift auto2fa-mac/Auto2FA/Views/Components/TunnelRow.swift
git commit -m "feat(ui): direct-mode target picker in New Tunnel sheet + direct-aware tunnel row"
```

---

## Final verification

- [ ] **Run the full Rust suite once more**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test 2>&1 | tail -10`
Expected: all pass.

- [ ] **Confirm the app builds clean**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -configuration Debug -destination 'platform=macOS' build 2>&1 | tail -5`
Expected: `** BUILD SUCCEEDED **`.

Then proceed to `superpowers:finishing-a-development-branch`.
