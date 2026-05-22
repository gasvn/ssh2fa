# Auto2FA Two-Layer Port Forward (Tunnels) — Design

**Date:** 2026-05-22
**Status:** Approved for implementation planning

## 1. Problem

Auto2FA today manages SSH master connections to login hosts (e.g. `k1`, `k2`, `k8`) and exposes them via a TUI dashboard. Users frequently need a second-layer port forward to a SLURM compute node, e.g.:

```
ssh -J shgao@k8 -L 8090:localhost:8090 shgao@holygpu8a11103.rc.fas.harvard.edu
```

Today this is run by hand. The compute node changes (jobs end, new jobs start), so the command must be retyped often. The user wants:

- Easy creation of named, persistent forwards.
- Fast selection of a compute node from the user's currently-running SLURM jobs (`squeue -u $USER`).
- Auto-recovery when the compute node disappears (mark stale, prompt for repick).
- Independence from any *specific* login host — any connected jump candidate is fine.
- A modern, easy-to-use UI integrated into the existing TUI dashboard.

## 2. Scope

In scope:
- Top-level "tunnels" feature integrated into the existing dashboard.
- Persistent config in a new `tunnels.json` next to `passwords.json`.
- SLURM `squeue`-based node discovery via the existing SSH masters.
- Lifecycle: create, start/stop, auto-start, failover between jump candidates, stale detection, delete.
- New unit tests in `tests/test_tunnels.py`.

Out of scope:
- Non-SLURM schedulers (qstat, kubectl, etc.). Picker offers manual node entry as escape hatch.
- Remote-port-different-from-local-port (defer; user said "by default 1" — same port).
- Dynamic SOCKS proxy / `-D` forwards.
- Reverse tunnels (`-R`).

## 3. Decisions (locked in during brainstorming)

| # | Decision | Rationale |
|---|---|---|
| 1 | Multiple **named** tunnels per user, not per host. | User confirmed they need a few named forwards (e.g. `jupyter`, `tboard`). |
| 2 | Tunnels are **top-level**, not nested under a specific host. | User: "I don't care which k I'm jumping through; they're interchangeable." |
| 3 | UI: dashboard split into a **Hosts** section on top and a **Tunnels** section below; cursor moves through both, `Tab` jumps sections. | User preferred integrated layout over modal or side panel. |
| 4 | When a compute node disappears, mark the tunnel **stale** and wait for user to repick. | User chose "stale, wait for me to repick" — most controllable, never silently routes to wrong host. |
| 5 | New-tunnel form asks only **name + local port** (remote port defaults to local). Local port validated for availability; prompt to change if busy. | User wanted the fastest possible flow. |
| 6 | On dashboard startup, attempt last-known node; if it's no longer in `squeue`, mark stale. | Best continuity without silent misrouting. |
| 7 | Jump candidates default to **every host in `passwords.json`**. Tunnel picks the first connected candidate as active jump. | User doesn't care which jump; only restrict when explicitly needed. |
| 8 | If the active jump dies, **silently fail over** to the next connected candidate (same compute node target). | Compute node, not jump, is what the user cares about. |
| 9 | Architecture: a `TunnelManager` class in a new `auto2fa/tunnels.py`, owning all tunnel state. `backend.py` exposes one read-only `is_master_ready()` helper. | Keeps `backend.py` from growing further; one clear ownership boundary. |
| 10 | Persistence in a separate `tunnels.json` (not in `passwords.json`). | Avoids rewriting credentials on every tunnel edit. |

## 4. Data Model

### 4.1 Persisted: `$SSH_CONFIG_PATH/tunnels.json`

```json
{
  "tunnels": {
    "jupyter": {
      "local_port": 8888,
      "remote_port": 8888,
      "jump_candidates": ["k1", "k2", "k8"],
      "last_node": "holygpu8a11103.rc.fas.harvard.edu",
      "last_user": "shgao",
      "auto_start": true
    },
    "tboard": {
      "local_port": 6006,
      "remote_port": 6006,
      "jump_candidates": null,
      "last_node": null,
      "last_user": null,
      "auto_start": false
    }
  }
}
```

Field semantics:
- `jump_candidates: null` ⇒ "use every host in `passwords.json`" (default for tunnels created via the New-Tunnel modal).
- `last_node: null` ⇒ tunnel has never had a node picked (or was reset).
- `auto_start: true` ⇒ on dashboard startup, attempt to start this tunnel.
- `remote_port` defaults to `local_port` when the form is filled, but is stored explicitly so future UI can edit it.

Atomic write: serialize to `tunnels.json.tmp`, then `os.replace` over `tunnels.json`. Reads tolerate a missing file (treated as empty config) but log and refuse to overwrite if the file exists but is unparseable.

### 4.2 In-memory: `TunnelState` (dataclass in `tunnels.py`)

```python
@dataclass
class TunnelState:
    name: str
    local_port: int
    remote_port: int
    jump_candidates: Optional[list[str]]   # None ⇒ all hosts
    last_node: Optional[str]
    last_user: Optional[str]
    auto_start: bool

    # runtime-only
    status: str = "idle"            # idle | starting | alive | stale | port_busy | failed
    active_jump: Optional[str] = None
    child: Optional["pexpect.spawn"] = None
    last_msg: str = "Ready"
    last_probe_ts: float = 0.0
    consecutive_squeue_misses: int = 0
```

### 4.3 Discovery payload: `Job`

```python
@dataclass
class Job:
    jobid: str
    partition: str
    name: str
    state: str    # always "RUNNING" after filter
    time: str     # e.g. "23:58:16" or "1-21:29:48"
    node: str     # NODELIST(REASON) field, raw
```

## 5. Components

### 5.1 New file: `auto2fa/tunnels.py`

Three concerns in one module:

**`TunnelState`** — the dataclass above.

**`NodeDiscovery`** (stateless functions)
- `discover(host_manager) -> list[Job]`
  - Builds: `ssh -o ControlPath=<active master> <host> "squeue -h -o '%i|%P|%j|%T|%M|%R' -u $USER"`
  - 5s timeout. Non-zero exit → raises `DiscoveryError(stderr)`.
- `parse(stdout: str) -> list[Job]`
  - Splits on `\n`, then on `|`. Drops rows where `state != "RUNNING"`.
  - Pure function for easy testing against canned `squeue` output.

**`TunnelManager`** — owns the dict of tunnels and runs the lifecycle:
- `__init__(self, host_managers: dict[str, SSHHostManager], config_path: str)`
- `load()` / `save()` — atomic JSON IO
- `add(name, local_port, remote_port=None, jump_candidates=None) -> TunnelState`
  - Validates: name not present; port is numeric in 1024–65535; port can `socket.bind(("127.0.0.1", port))`. Raises `ValueError` with a human message.
- `remove(name)`
- `start(name)` — picks active jump, spawns ssh process. Idempotent (no-op if already alive).
- `stop(name)` — terminates child, sets `status = idle`.
- `toggle(name)`
- `set_node(name, node, user)` — called by the picker. Persists; if tunnel was stale/idle, calls `start`.
- `pick_active_jump(state) -> Optional[str]` — iterates `jump_candidates or all_hosts`, returns first where `host_managers[name].is_master_ready()` is True.
- `tick()` — one health pass over all tunnels. Cheap (no network) for `idle` tunnels; runs squeue/probe checks for `alive` ones on a longer cadence (every 30s). Idempotent.
- `cleanup_orphans()` — on startup, `pgrep -f "ssh -J .* -L <our_local_ports>:"` and kill leftovers from a prior run.
- `shutdown()` — called on dashboard exit: stop all tunnels.

### 5.2 Hook in `auto2fa/backend.py`

One new method on `SSHHostManager`:

```python
def is_master_ready(self) -> bool:
    return self.active and self.pool_status.get(self.active_index) == "Ready"
```

No other changes to existing logic. The connection pool, heartbeat, and rotation continue exactly as they are.

### 5.3 `auto2fa/main.py` integration

- After host managers are created and started, instantiate `TunnelManager(host_managers, config_path)` and call `tunnels.load()` then `tunnels.cleanup_orphans()`. Any tunnel with `auto_start: true` is queued for a `start()` attempt on the first `tick()` (after host masters have had a chance to come up).
- The render loop becomes a two-table layout: Hosts table on top (unchanged), Tunnels table on bottom (new). One shared cursor; `Tab` jumps sections.
- New keys (only active when cursor is on a tunnel row, except `T`):
  - `T` — open New-Tunnel modal (any section)
  - `Enter` — open node picker for the selected tunnel
  - `Space` — start/stop selected tunnel
  - `D` — delete selected tunnel (with `Y/N` confirm)
- `tunnel_manager.tick()` is called once per render iteration (10 Hz). Inside `tick()`, the expensive checks self-throttle.
- On exit, `tunnel_manager.shutdown()` runs alongside the existing manager cleanup.

## 6. UI

### 6.1 Main dashboard

```
╭─ Auto2FA ──────────────────────────────────────────────────────╮
│ HOSTS                                                          │
│   Host       Status         Pool   FS    Last Message          │
│ ▶ k8         ● Connected    0/2    —     Ready                 │
│   k1         ● Connected    0/2    —     Ready                 │
│   login05    ○ Stopped      —      —     Inactive              │
│────────────────────────────────────────────────────────────────│
│ TUNNELS                                                        │
│   Name       Local  →  Node                  Via    Status     │
│   jupyter    :8888  →  holygpu8a11103        k8     ● alive    │
│ ▶ tboard     :6006  →  holygpu8a15203        —      ○ stale    │
│   vscode     :7777     (no node yet)         —      ○ idle     │
╰────────────────────────────────────────────────────────────────╯
 [↑↓] Nav   [Tab] Switch section   [Space] Toggle   [T] New tunnel
 [⏎] Pick node   [D] Delete tunnel   [R] Rotate pool   [Q] Quit
```

Status indicators:

| Status | Glyph | Color | Meaning |
|---|---|---|---|
| `alive` | `●` | green | ssh process up, port bound, jump healthy |
| `starting` | `◐` | yellow | spawning / waiting for forward to bind |
| `stale` | `○` | dim red | node no longer in squeue; needs repick |
| `idle` | `○` | dim | never started, or stopped |
| `port_busy` | `●` | red | local port held by another process |
| `failed` | `●` | red | ssh process exited; see `last_msg` |

### 6.2 New-tunnel modal (`T`)

```
╭─ New Tunnel ──────────────────────────╮
│  Name:        jupyter_                │
│  Local port:  8888                    │
│                                       │
│  [Enter] Create  [Esc] Cancel         │
╰───────────────────────────────────────╯
```

- `Tab` switches between fields.
- On `Enter`: validate. Errors render inline in red below the field:
  - `"Name already exists"` — fix name.
  - `"Port must be 1024–65535"` — fix port.
  - `"Port 8888 in use, try another"` — fix port.
- On success: persist, close modal, cursor jumps to the new row. Tunnel sits in `idle` until user presses `Enter` to pick a node.

### 6.3 Node picker (`Enter` on tunnel row)

```
╭─ Pick compute node for "jupyter" via k8 ─────────────────────╮
│   #  JobID      Partition    Name      Time         Node      │
│   1  14246008   kempner_h    h100x1    23:58:16     holygpu8a11103  │
│ ▶ 2  13756572   kempner_h    h100x1    1-21:29:48   holygpu8a15203  │
│   3  12975569   kempner      a100x1    5-16:13:17   holygpu8a19403  │
│                                                               │
│   ↻ Refresh (R)   ⌨  Custom node…  (C)    Esc Cancel          │
╰───────────────────────────────────────────────────────────────╯
 [↑↓] Pick  [⏎] Use this node  [R] Refresh squeue  [C] Type manually
```

- Header shows which jump is being used (so the user knows where `squeue` is being run).
- Empty squeue → renders `"No running jobs found."`, `C` still available.
- `squeue` failure → renders `"squeue failed: <stderr>"`, `C` still available.
- `C` opens a tiny text-input row for manual node entry (e.g. `holygpu8a11103.rc.fas.harvard.edu`).
- On `Enter`: `set_node(name, node, user)`. The user field is derived from `ssh -G <jump> | grep ^user` (the SSH config) so the user rarely has to think about it.

### 6.4 Failover (no modal, surfaced in `last_msg`)

- Jump dies → tunnel respawns through next connected candidate. Row updates: `via k8` → `via k1`, `last_msg = "failover k8→k1"`.
- 2 consecutive `squeue` misses (60s) → mark `stale`, kill ssh process, row turns red-dim. User presses `Enter` to repick.

## 7. Lifecycle Details

### 7.1 Start

1. If already `alive` or `starting`, return.
2. If `last_node` is `None` → `status = idle`, `last_msg = "no node — press Enter to pick"`, return. (Triggered when user hits `Space` on a freshly-created tunnel.)
3. `active_jump = pick_active_jump(state)`. If None → `status = idle`, `last_msg = "waiting for jump"`, return.
4. Port re-check: `socket.bind(("127.0.0.1", local_port))`. If fails → `status = port_busy`, `last_msg = "port N in use"`, return.
5. `status = starting`. Spawn:
   ```python
   pexpect.spawn(
       "ssh",
       ["-N",
        "-J", active_jump,
        "-L", f"{local_port}:localhost:{remote_port}",
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "ExitOnForwardFailure=yes",
        "-o", "ServerAliveInterval=15",
        f"{last_user}@{last_node}"],
       encoding="utf-8",
       timeout=15,
   )
   ```
   `-N` = no remote command; pure forward.
   `ExitOnForwardFailure=yes` ensures the child exits fast if the bind fails on the remote side.
   `last_user` defaults to the jump host's SSH config user (`ssh -G <jump> | grep ^user`) when first picked — for Harvard RC and similar clusters the compute-node user is always the same as the login user. Stored explicitly so an advanced user could edit `tunnels.json` if it ever differs.
6. Probe loop (≤10s): every 200ms, attempt `socket.connect(("127.0.0.1", local_port))`. On success → `status = alive`, `last_msg = "via {jump}"`. On timeout → kill child, `status = failed`, parse `child.before` for a short reason.

### 7.2 Tick (every render frame, ~100ms; expensive checks self-throttle)

For each tunnel:
- `idle` / `port_busy` / `failed` — no-op.
- `starting` — handled inside `start()`'s probe loop; tick skips.
- `alive`:
  - If `child` is dead → respawn via `start()`.
  - If `active_jump`'s master is no longer ready → kill child, call `start()` (picks a new jump).
  - Every 30s, run `NodeDiscovery.discover(active_jump_mgr)`. If `last_node` is missing from the list, increment `consecutive_squeue_misses`. When it reaches 2 → mark `stale`, kill child. If found, reset counter to 0.
  - `discover` errors don't bump the miss counter (could be transient); they're logged and `last_msg` shows a hint.
- `stale` — no-op until user repicks (which calls `set_node` → `start`).

### 7.3 Stop / Delete

- Stop: send SIGTERM, wait up to 2s, SIGKILL if still alive. Clear `child`, `active_jump`. `status = idle`.
- Delete: confirm modal, then stop + remove from dict + `save()`.

### 7.4 Startup

1. `load()` — read `tunnels.json`. Apply all configs to `tunnels` dict in `idle` state.
2. `cleanup_orphans()` — pgrep + kill any leftover `ssh -J … -L <our_ports>:…` from prior runs.
3. Record `startup_ts = time.time()`. In each `tick()`, while `time.time() - startup_ts < 3`, skip auto-start (lets masters come up). On the first tick after the 3s grace period, iterate every tunnel with `auto_start: true` and `last_node` set, and call `start()`. The flag is then cleared so auto-start is attempted only once per dashboard run; if the jump isn't ready yet at t=3s the tunnel sits in `idle` and the user can `Space` to retry. The normal flow handles "node gone" → marks stale; no special case.

## 8. Error Handling

| Failure | Detection | Resulting state |
|---|---|---|
| Local port busy at start | `socket.bind` pre-check or `bind: Address already in use` in stderr | `port_busy`, modal blocks creation |
| No connected jump candidate | `pick_active_jump` returns None | `idle`, `last_msg = "waiting for jump"` |
| Compute node SSH rejects | pexpect sees `Permission denied`/`Host key`/EOF in ≤15s | `failed`, `last_msg = "auth failed: <reason>"` |
| Forward bind fails on remote | child exits fast (`ExitOnForwardFailure`) | `failed`, `last_msg = "remote bind failed"` |
| Compute node unreachable from jump | pexpect sees `channel … open failed` / `No route to host` | `failed`, `last_msg = "node unreachable"` |
| Node gone from squeue (2 misses) | tick discovery check | `stale`; user repicks |
| squeue fails | non-zero exit code on jump | Picker shows `"squeue failed: <stderr>"`; manual entry still works |
| Dashboard killed mid-session | n/a | Startup `cleanup_orphans()` reaps; tunnels reload from JSON |
| `tunnels.json` malformed | JSON parse error | Log error; do **not** overwrite. Start with empty in-memory config; user can fix the file manually |
| Concurrent writes | Atomic tmp+rename | No partial writes |

## 9. Edge Cases

- **Renamed host in `passwords.json`**: tunnels referencing the old name as a candidate simply find it not connected. They wait. Log a warning once per startup per missing candidate.
- **Deleted host in `passwords.json`**: same as above; no cascading tunnel deletion.
- **Two tunnels on the same port**: prevented at creation; re-checked at every `start` (something else may have grabbed it between launches).
- **`squeue` returns a node range (e.g. `holygpu[01-03]`)**: picker shows the raw string; if user picks it, we use the first node (`holygpu01`) and append `(range)` to `last_msg`.
- **TTY recovery on modal crash**: modal input runs inside the same `RawMode` context manager already used by the main loop; an exception unwinds cleanly without scrambling the terminal.
- **VSCode / IDE workflow**: once a tunnel is alive, `localhost:<port>` works transparently. README will be updated to call this out.

## 10. Testing

### 10.1 `tests/test_tunnels.py` (new, unit-only — no real SSH)

Follows the style of the existing `tests/test_pooling_logic.py` (mocking `subprocess` and `pexpect`).

- **`TunnelManager.add`** — name uniqueness, port range validation, port-available check (using a real `socket.bind` test against a port we hold open), persistence side-effect.
- **`TunnelManager.load` / `save`** — round trip; malformed JSON does not destroy the file; atomic write survives a simulated mid-write crash (`os.replace` raises).
- **`pick_active_jump`** — given mocked host managers in various states (none up, all up, candidates filtered, candidate not in passwords): returns the right one or None.
- **`NodeDiscovery.parse`** — against canned `squeue` output including the user's exact example, empty output, malformed rows, mixed-state rows.
- **`tick` state machine** — drive transitions with stubs:
  - `idle → starting → alive`
  - `alive → (jump master goes down) → starting → alive(via different jump)`
  - `alive → (squeue miss x2) → stale`
  - `alive → (child dies) → starting → alive`
  - `idle → port_busy` (port held during start)
- **Atomic JSON write** — mock `os.replace` to raise; assert original file is intact.

### 10.2 Manual smoke test (recorded in spec, not automated)

1. Create tunnel `jupyter` :8888, pick a node, confirm `curl localhost:8888` reaches the service.
2. `kill -9` the underlying `ssh -N` process; confirm tick respawns it within ~1s.
3. Stop `k8` in dashboard while tunnel is alive; confirm failover to `k1`, `last_msg = "failover k8→k1"`.
4. `scancel` the SLURM job; confirm tunnel goes stale within 60s.
5. Quit dashboard, relaunch; confirm tunnel comes back up using last-known node (or stales if job is gone).
6. Try to create a tunnel on a port held by another process; confirm inline modal error and no JSON write.
7. Delete a tunnel; confirm `tunnels.json` is updated and orphan-reaped on next launch.

## 11. Open Questions

None — all design questions were resolved during brainstorming. Implementation plan to be drafted next.
