# Direct-Mode Port Forwarding — Design Spec

**Date:** 2026-06-19
**Status:** Approved direction (explicit `direct_host` field + `ForwardSpec` enum), pending spec review
**Goal:** Let a tunnel forward a local port straight to a service running on a **registered host itself** — `ssh -N -L <local>:localhost:<remote> <host>` — with no jump host and no SLURM compute node, reusing the host's warm ControlMaster so it connects instantly with no 2FA.

---

## 1. Context & problem

Today every tunnel is a **two-hop SLURM forward**: `ssh -N -J <jump> -L <local>:localhost:<remote> <user>@<node>` (`forward.rs::build_forward_argv`). Both `tunnel_start` (IPC) and `do_tunnel_start` (maintenance recovery/boot) refuse to start a tunnel until it has BOTH a ready jump host AND a `last_node` (set via the node picker / squeue discovery). There is no way to forward a port to a service that runs **on the login host itself** (e.g. a Jupyter or web UI on the host you already log into) — the user is forced through a node picker that has no relevant node.

Direct mode adds that path: target = a registered host's own `localhost:<remote>`.

## 2. Design principles

- **Explicit over convention.** A new `direct_host: Option<String>` field marks the mode; `Some(host)` = direct, `None` = today's compute behavior. No "empty jump = direct" heuristics.
- **Reuse the warm master.** Direct forwards run `ssh <host>` where `<host>` is the ssh-config alias — the same alias the daemon's ControlMaster bound — so ssh multiplexes over the existing socket: instant, no new 2FA. Identical mechanism to how the compute path's `-J <jump>` reuses the jump's master.
- **Backward compatible.** `#[serde(default)]` → existing `tunnels.json` and existing daemon snapshots load `direct_host = None`; every current tunnel is byte-for-byte unaffected.
- **Compiler-enforced modes.** A `ForwardSpec` enum (`Compute { jump, user, node }` | `Direct { host }`) replaces the bare `(jump, user, node)` argument triple in the two start-workers, so the compiler forces both modes to be handled in **both** duplicated workers (the class of bug where one path is wired and the other forgotten).
- **YAGNI.** Direct-ness is set at **creation only** (via `tunnel_add`). No mid-life mode switch, no `tunnel_set_direct` handler. To change, delete + re-add.

## 3. Architecture

Direct mode threads a single new persisted field through the existing start/maintenance plumbing. Files touched:

**Rust core (`a2fa-core`)**
- `model/tunnel.rs` — add `direct_host: Option<String>` (persisted, `#[serde(default)]`).
- `tunnels/forward.rs` — add `ForwardSpec` enum, `build_direct_argv`, `start_forward_direct`, `start_forward_spec` dispatch. Keep `build_forward_argv` / `start_forward` (compute) and their tests intact.

**Rust daemon (`a2fa-daemon`)**
- `handlers/tunnels.rs` — `tunnel_add` accepts optional `direct_host`; `tunnel_snapshot` emits it; `tunnel_start` branches direct vs compute and builds a `ForwardSpec`.
- `workers.rs` — `spawn_tunnel_start` / `spawn_tunnel_start_with_runtime` / `spawn_tunnel_start_inner` take a `ForwardSpec` instead of `(jump, user, node)`.
- `tunnel_maintenance.rs` — `TunnelSnapshot` carries `direct_host`; `do_tunnel_start` branches; its private `spawn_tunnel_start_with_runtime` takes a `ForwardSpec`; `process_tunnel` / `run_boot_autostart` pass `is_direct` into the decision functions.
- `tunnel_runtime.rs` — `tunnel_action` and `should_autostart` gain an `is_direct: bool` parameter that suppresses the SLURM-only behaviors (squeue checks, node-required boot gate).

**Swift app**
- `Models/Tunnel.swift` — add `directHost: String?` + `var isDirect`.
- `BackendClient.swift` — `addTunnel` passes `direct_host`.
- `AppState.swift` — `createTunnel` passes `directHost` through.
- `Views/NewTunnelSheet.swift` — a "Target" picker: **Compute node (SLURM)** (default) vs **Direct to a host** + a registered-host picker.
- `Views/Components/TunnelRow.swift` — direct rows show `→ host · direct`, and hide the node-picker / jump-host / countdown affordances.

## 4. The `direct_host` field & wire format

`Tunnel` (Rust) gains, in the persisted block:

```rust
/// When `Some(host)`, this tunnel forwards local_port → localhost:remote_port
/// ON that registered host directly (ssh -N -L … <host>), with NO jump host
/// and NO SLURM compute node. `None` = the default SLURM compute-node forward.
#[serde(default)]
pub direct_host: Option<String>,
```

`tunnel_snapshot` adds `"direct_host": t.direct_host`. Swift `Tunnel` decodes `direct_host` → `directHost: String?` via `decodeIfPresent` (older snapshots → `nil`), and exposes `var isDirect: Bool { directHost != nil }`.

## 5. `ForwardSpec` (forward.rs)

```rust
/// What a forward connects to. Compute = SLURM two-hop; Direct = the host's own localhost.
#[derive(Debug, Clone)]
pub enum ForwardSpec {
    Compute { jump: String, user: String, node: String },
    Direct  { host: String },
}

impl ForwardSpec {
    /// Host label for logs / UI / the tunnel's `active_jump` field
    /// (the jump for compute, the host for direct).
    pub fn label(&self) -> &str {
        match self { Self::Compute { jump, .. } => jump, Self::Direct { host } => host }
    }
}
```

`build_direct_argv(host, local, remote)` → `["-N", "-L", "<local>:localhost:<remote>", <SSH_OPTS…>, host]` — **no `-J`, no `user@node`**. Reuses the same `SSH_OPTS` constant as the compute argv.

`start_forward_direct(host, local, remote)` mirrors `start_forward`: reject a leading `-` in `host` (argument-injection guard), then spawn `ssh` with `build_direct_argv`, all three stdio set to `null` (same rationale as `start_forward`).

`start_forward_spec(&spec, local, remote)` dispatches: `Compute → start_forward(jump,user,node,…)`, `Direct → start_forward_direct(host,…)`. The two workers call **only** `start_forward_spec`.

## 6. Daemon start paths

Both start sites resolve a `ForwardSpec` under the State lock, then spawn off-lock.

**`tunnel_start` (handlers/tunnels.rs)** — after the existing `Alive | Starting` idempotency latch, read `direct_host`:

- **Direct (`Some(host)`):** verify a registered host named `host` exists with `is_master_ready == true`.
  - Not ready → set `status = Idle`, `last_msg = "waiting for host <host>"`, `active_jump = Some(host)`, `wants_alive = true`, return `Ok(Null)` (maintenance recovery retries when the master comes up — same shape as the compute "waiting for jump host" path).
  - Ready → set `status = Starting`, `active_jump = Some(host)`, `last_msg = "starting direct to <host>"`, `wants_alive = true`; spec = `Direct { host }`.
  - **No node / jump-candidate / user resolution and no squeue.**
- **Compute (`None`):** today's logic, unchanged, producing `Compute { jump, user, node }`.

**`do_tunnel_start` (tunnel_maintenance.rs)** — the same branch (recovery + boot use this), using the snapshot's `direct_host`.

**Workers** (`workers.rs` + maintenance's private copy): take `spec: ForwardSpec` in place of `(jump, user, node)`. Inside:
- log / `active_jump` use `spec.label()`.
- the connect record uses a mode-aware target string: `Compute → "<node>:<remote>"`, `Direct → "<host>:<remote> (direct)"`.
- `run_post_connect` needs a `(node, jump)` pair → `Compute → (node, jump)`, `Direct → (host, host)`.
- abort / store_child / probe / failure handling are mode-agnostic (keyed by tunnel name) and unchanged.

## 7. Maintenance gating (`tunnel_action`, `should_autostart`)

Direct tunnels have no SLURM job, so the squeue machinery must never run for them. Add `is_direct: bool` to the two pure decision functions:

- **`tunnel_action(…, is_direct)`** — when `is_direct`, the two `SqueueCheck` branches are suppressed:
  - down + `wants_alive` (`Idle|Failed|PortBusy`): skip the squeue branch → fall straight to the recovery throttle (`Recover` / `Skip`).
  - `Alive`: skip the squeue-due branch (only `StopDead` for child-died/port-gone and `StopDisabledJump` for a disabled host remain).
  - `Stale if wants_alive → Skip` and all other arms are unchanged (a direct tunnel never reaches `Stale` since nothing marks it so).
- **`should_autostart(auto_start, wants_alive, last_node, is_direct)`** → `(auto_start || wants_alive) && (last_node.is_some() || is_direct)`. A direct tunnel (no `last_node`) is boot-eligible on its `wants_alive` / `auto_start` flag alone.

`process_tunnel` and `run_boot_autostart` pass `snap.direct_host.is_some()`. `TunnelSnapshot` gains a `direct_host: Option<String>` field (populated in both snapshot sites).

`run_squeue_check` already early-returns when `last_node` is `None`, so it is doubly safe — but with the gate above it is never dispatched for a direct tunnel in the first place.

**`active_jump` reuse:** for a direct tunnel `active_jump = Some(host)`. The maintenance `jump_host_active` lookup therefore resolves the direct host's `active` flag, so disabling the host correctly triggers `StopDisabledJump`. This is intentional reuse, not a special case.

## 8. `tunnel_add` (creation)

`tunnel_add` accepts an optional `direct_host` string param:
- absent / `null` / empty → `None` (compute tunnel, today's behavior).
- present → trimmed; reject a leading `-` (injection guard, mirrors `start_forward_direct`) with `BadParams`; stored verbatim on the new `Tunnel`.

No strict "must be a known host" check at add time (the host may not be registered yet, and the UI already picks from registered hosts) — readiness is enforced at start, exactly like `last_node` / jump resolution. The created tunnel's snapshot carries `direct_host`.

## 9. UI

**NewTunnelSheet** — add a **Target** segmented picker above Name (or below Template):
- **Compute node (SLURM)** (default) → today's flow; `directHost = nil`.
- **Direct to a host** → a `Picker` over `appState.hosts` (host name), bound to `@State selectedHost`. Name / Local port / auto-start fields are unchanged and still apply. The node picker is simply never entered for direct tunnels.
- On Create: `createTunnel(…, directHost: target == .direct ? selectedHost : nil)`. If Direct with no host selected (no registered hosts), show the inline error "Add a host first, then forward a port to it."

**TunnelRow** — when `tunnel.isDirect`:
- the node column renders `→ <directHost>` with a small `· direct` caption instead of the node / `(no node)` text.
- the `via <jump>` menu is replaced by a static `direct` label (direct tunnels have no jump candidates).
- the **Node** affordances are hidden: the hover-bar "Node" button, the overflow "Pick node…" item, the failed-state "Node" recovery button, and the "Use jump host" submenu are each gated on `!tunnel.isDirect`.
- Start / Stop / Open-in-browser / Copy / Details / Rename / Clone / Delete are unchanged.
- the SLURM walltime countdown naturally never shows (no `TunnelDeadlines` entry for a direct tunnel).

`BackendClient.addTunnel(name:localPort:remotePort:directHost:)` adds `params["direct_host"] = directHost` when non-nil. `AppState.createTunnel(…, directHost:)` forwards it.

## 10. Edge cases

| Case | Handling |
|------|----------|
| Direct host not yet master-ready at start | `tunnel_start` returns `Idle` + "waiting for host"; maintenance recovers it when the master comes up (`should_autostart` direct-eligible). |
| User disables the direct host | `active_jump == host` → `StopDisabledJump` stops the tunnel (desired). |
| Direct host's master dies / port collision | `StopDead` / `ChildExited` paths fire exactly as for compute; recovery rebuilds a `Direct` spec. |
| Old `tunnels.json` / old daemon snapshot | `#[serde(default)] / decodeIfPresent` → `direct_host = None`; unchanged compute behavior. |
| Direct selected but no registered hosts | NewTunnelSheet blocks Create with an inline "add a host first" message. |
| `direct_host` starts with `-` | `tunnel_add` rejects with `BadParams`; `start_forward_direct` rejects as a second guard. |
| `tunnel_set_node` called on a direct tunnel | Harmless: it records `last_node`, but `tunnel_start` branches on `direct_host` first and ignores it. UI never exposes the node picker for direct tunnels. |

## 11. Testing

**Rust (unit, `cargo test`):**
- `forward.rs`: `build_direct_argv` shape — contains `-N`, contains `"<lp>:localhost:<rp>"`, **last arg is the bare host**, **no `-J`**, carries `ExitOnForwardFailure` / `StrictHostKeyChecking=no`; `start_forward_direct` rejects a leading-`-` host.
- `tunnel_runtime.rs`: `tunnel_action(is_direct=true)` — down + `wants_alive` with squeue due → `Recover` (NOT `SqueueCheck`); `Alive` + squeue due → `Skip`. `should_autostart(_, true, None, /*is_direct=*/true)` → `true`; `should_autostart(_, true, None, false)` → `false` (existing). Existing `tunnel_action` / `should_autostart` tests get the new arg threaded through (compute = `false`).
- `handlers/tunnels.rs`: `tunnel_add` with `direct_host` stores it and the snapshot carries it; `tunnel_add` with a leading-`-` host → `BadParams`; `tunnel_start` on a direct tunnel with **no ready host** → `Idle` + "waiting for host" (no spawn — mirrors the existing `tunnel_start_no_node_sets_idle_last_msg` no-spawn style).

**Swift:** the `directHost` decode is covered by the existing snapshot-decode path (add a direct field to a decode fixture if one exists); the NewTunnelSheet direct flow + TunnelRow rendering are build-gated + manual QA (first-class compute create still works; direct create → row shows `→ host · direct`, no node/jump affordances, starts instantly over the warm master).

## 12. Out of scope (v1)

- No mid-life mode switching (`tunnel_set_direct`); direct-ness is creation-time only.
- No remote-bind / `-R` reverse forwards; only local `-L`.
- No forwarding to a third arbitrary `host:port` (the YAGNI scope is `localhost` on the registered host itself — confirmed with the user).
- No merge of the two duplicated start-workers (`workers.rs` vs `tunnel_maintenance.rs`) — out of this feature's scope; we only thread `ForwardSpec` through both.
