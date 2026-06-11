# Design — SSH connection robustness: kill the health-check thrash + single stable master

Date: 2026-06-11
Status: approved (direction); awaiting spec review → implementation plan
Scope: `auto2fa-rs/` daemon + core (the ControlMaster lifecycle), with a small
follow-on UI/IPC simplification in `auto2fa-rs` clients and `auto2fa-mac`.

## Goal

Stop the daemon from killing the user's own SSH sessions and re-running 2FA in a
loop. Make the per-host master a single, stable, long-lived connection whose
liveness is judged cheaply and conservatively, so transient load never triggers a
destructive reconnect.

## Root cause (diagnosed from the live system, 2026-06-11)

The daemon was thrashing every host in a tight loop. The exact chain, from
`/tmp/auto2fa_daemon.log`:

1. The heartbeat probes each master every few seconds with `ssh -O check`
   (`master_check`, bounded 5 s in `control.rs`).
2. Under machine/cluster load that probe does not answer within 5 s — *not*
   because the master is dead, but because (a) the mux master's control channel
   blocks while the underlying TCP is stale (a documented OpenSSH behavior), and
   (b) the daemon is so saturated spawning probe/`pgrep`/login subprocesses that
   even a local `pgrep` exceeds its 2 s bound (`run_cmd_bounded: pgrep exceeded
   2s — killing` appears constantly).
3. `master_check` collapses "timed out" and "genuinely dead" into the same
   `false`, and `next_action` (`managers.rs:309`) condemns a Ready slot on a
   **single** failed check — no hysteresis:
   ```rust
   SlotStatus::Ready => check_alive == Some(false),  // one blip ⇒ "restart"
   ```
4. The restart path runs a full 2FA re-login (hammering FAS-RC) **and** sweeps
   the old master process (`cleanup_stale_socket` → `kill_orphaned_master`,
   SIGKILL). Because the user's interactive sessions are multiplexed onto that
   master (`ControlMaster no` + `ControlPath ~/.ssh/cm-auto2fa-%h`), killing it
   **drops every one of their sessions at once** — confirmed by FASRC's own docs:
   "if you exit or kill the initial connection *all* other ones die, too."
5. The very next tick finds the master was alive all along
   (`marked Dead but master is ALIVE — adopting`) — proving the condemnation was
   a false positive. The loop self-amplifies until the daemon wedges hard enough
   to be SIGKILLed (`launchctl list` shows last exit `-9`) and relaunched, which
   re-establishes masters and churns again.

The 2-slot pool makes this worse: two masters per host, each independently
false-condemnable, with the user's sessions spread across both — double the kill
surface. The active-symlink rotation also strands existing sessions on the old
socket inode when it flips.

**The bug is self-inflicted. The fix is to make liveness judgement cheap,
non-blocking, and conservative, and to make teardown incapable of killing a live
connection.**

### Research backing

- `ssh -O check` / mux control commands block when the underlying connection is
  stale (nixCraft SSH multiplexing guide; Azure/azure-cli-extensions#7285). So
  polling it on the hot path is the wrong probe.
- A control socket's liveness can be tested without spawning ssh: the socket file
  exists (`test -S`) and a master is listening (craigvantonder gist).
- Network-death detection is the job of `ServerAliveInterval` /
  `ServerAliveCountMax` on the master itself (FASRC docs; OpenSSH).

## Design principle: separate the two questions

Today one probe (`ssh -O check`) is (mis)used to answer two different questions.
Split them and give each to the mechanism built for it:

| Question | Owner | Mechanism |
|---|---|---|
| Is the **network** to the server alive? | the ssh master itself | `ServerAliveInterval=15` × `ServerAliveCountMax=12` (already set). On true death the master exits on its own. |
| Is the **master process** still here to serve mux clients? | the daemon heartbeat | a cheap, fork-free, non-blocking probe (below) |
| Deep confirmation (rare) | the daemon, cold path | `ssh -O check`, slow cadence only, never in the hot loop |

The daemon stops trying to judge network health. It only asks "is a master
listening at this path?" — and only acts when the answer is a confident, repeated
"no."

## Architecture changes

### 1. Single stable master per host (retire the pool + symlink)

- One master per host, on a **fixed** ControlPath `cm-auto2fa-<host>` (the path
  the user's `ControlPath ~/.ssh/cm-auto2fa-%h` already resolves to). No `-<index>`
  suffix, no symlink, no rotation.
- The user's `~/.ssh/config` needs **no change**: today `cm-auto2fa-<host>` is a
  symlink to a slot socket; after this change it *is* the master's real socket.
- `start_master` spawns ssh with `ControlPath=<stable path>` directly. The argv is
  otherwise unchanged (`ControlMaster=auto`, `ControlPersist=yes`,
  `ServerAliveInterval=15`, `ServerAliveCountMax=12`, `ConnectTimeout=10`,
  `-E /tmp/auto2fa_ssh_master_<host>.log`).
- Removed concepts: `active_index`, `try_rotate`/rotation, active-symlink
  (`update_symlink`/`symlink_target_index`/`active_symlink_path`), `WarmSlot1`,
  rotation ping-pong / `probe_backoff`, the per-slot `-N` path.
- Retained breakers: per-host **cooldown** after N consecutive *login* failures
  (`OTP_FAILURE_THRESHOLD`/`OTP_COOLDOWN`) and **flap back-off** after repeated
  connect-then-drop (`FLAP_*`). These guard against a genuinely bad
  secret/host hammering FAS-RC and are unaffected by the thrash fix.

Trade-off (accepted): on a *genuine* master death there is no warm spare to fail
over to; the daemon re-authenticates in place (possibly a fresh OTP, same as a
first connect). With false positives eliminated, genuine deaths are rare, and on a
real network drop a spare would be dead too — so the spare's value did not justify
its complexity and doubled kill surface.

### 2. Cheap, non-blocking liveness probe (hot path)

New `master_probe(control_path) -> MasterLiveness` replacing `master_check` in the
heartbeat hot path:

- **`Alive`** — a non-blocking `connect()` to the unix-domain ControlPath
  succeeds. The kernel completes the connect against the listening socket even if
  the master's user-space event loop is momentarily busy — so this tests "the
  master process exists and its socket is open," not "the event loop is free right
  now." Fork-free, microseconds.
- **`Dead`** — the socket file exists but `connect()` returns `ECONNREFUSED` (no
  listener; master gone), or the socket file is absent (`ENOENT`).
- **`Inconclusive`** — any other/transient error, or a non-blocking connect that
  does not complete within a tiny timeout (e.g. `EAGAIN`/backlog). Treated as "not
  a confident answer" → never escalates on its own.

The probe immediately closes the connection (a bare connect/close on a SOCK_STREAM
mux socket is handled gracefully by the master as a client that disconnected
before its hello). It never speaks the mux protocol and never spawns a process.

`ssh -O check` is retained only for: (a) the cold reconfirmation path and (b)
parsing `Master running (pid=N)` when we want the pid for logging.

### 3. Hysteresis — condemn only on repeated, confident failure

Per-host state gains `consecutive_probe_failures: u32`.

- `Alive` → reset the counter to 0 (and feed flap "slot alive" bookkeeping).
- `Inconclusive` → leave the counter unchanged (neither confirms nor denies).
- `Dead` → increment.

A master is declared dead and a reconnect scheduled **only** when
`consecutive_probe_failures >= PROBE_FAILURE_THRESHOLD` (default **3**, i.e. ~3
ticks) **and** the host is active, not in cooldown, not in flap back-off. One blip
never triggers a reconnect.

### 4. Teardown that cannot kill a live connection

The session-killer was `cleanup_stale_socket`/`kill_orphaned_master` running on the
reconnect path. New rules:

- **Adopt-before-restart:** when a reconnect is about to run, re-probe first. If
  `connect()` now succeeds, the master is alive — **adopt it, do not kill, do not
  re-auth** (the cheap analogue of today's `AdoptAlive`, but it now also gates the
  kill).
- **Never SIGKILL a listening master.** Process-kill is reserved for *leaked*
  master processes that are **not** the current host's master (wrong/forgotten
  path) **and** have no live listener of their own. With a single stable path and
  no rotation, our own logic cannot create duplicates, so this path is essentially
  a boot-time janitor for pre-upgrade leftovers (see Migration).
- Removing a stale socket *file* is only done when `connect()` is `Dead`
  (no listener) — i.e. there is nothing live to disturb.

### 5. State / IPC / UI simplification

- `State.host`: keep `is_master_ready: bool` as the single source of truth. The
  IPC JSON keeps `pool_index` (always 0) and `pool_alive` (0 or 1) **for
  back-compat** so the existing Swift `Host` decoder and CLI/TUI keep parsing —
  but they no longer mean "pool".
- UI surfaces stop showing pool jargon:
  - Swift `HostRow.poolPips` ("x/2 connections ready") → a single connection
    indicator driven by `is_master_ready` (no "/2").
  - CLI `pool={idx}/{alive}` and TUI `{pool_index}/{pool_alive}` → a single
    connected/disconnected glyph.
  - `FriendlyText` mappings that mention "pool" stay (they already translate to
    friendly words) but are reviewed for accuracy.
- The manual `rotate` IPC handler (`hosts.rs:520-582`) is removed or made a no-op.

## Components (files)

- `crates/a2fa-core/src/ssh/control.rs`
  - Add `master_probe(&Path) -> MasterLiveness` (non-blocking unix connect).
  - Keep `master_check`/`master_owner_pid` for cold/pid use; drop their hot-path
    callers. Remove symlink helpers (`update_symlink`, `active_symlink_path`,
    `symlink_target_index`) and the `-<index>` form of `control_path`; `control_path`
    returns the stable base path.
  - `cleanup_stale_socket`/`kill_orphaned_master`: gate kills on "no live listener
    and not the active path."
- `crates/a2fa-core/src/ssh/master.rs`
  - `PoolState` → single-master state: drop `active_index`, rotation/ping-pong
    fields; add `consecutive_probe_failures`. Keep cooldown + flap fields.
    `POOL_SIZE` and slot arrays collapse to one master. (`mark_slot_ready`,
    `note_slot_alive`, `note_slot_dropped`, cooldown/flap APIs retained, de-indexed.)
  - `start_master` uses the stable path; success/failure bookkeeping de-indexed.
- `crates/a2fa-daemon/src/managers.rs`
  - `next_action` collapses to: `Skip` (inactive / cooldown / flap back-off) |
    `Restart` (probe `Dead` and `consecutive_probe_failures >= THRESHOLD`) |
    `AdoptAlive` (state says down but probe `Alive`) | `Healthy`. Remove
    `WarmSlot1`/`Rotate`. Keep it a **pure** function (unit-tested).
  - `tick_host` uses `master_probe`, updates the failure counter, and keeps the
    in-flight `StartGuard`, off-thread restart worker, and toggle-off races intact.
- `crates/a2fa-daemon/src/{workers.rs,handlers/hosts.rs}`
  - De-index pool writes; set `pool_index=0`, `pool_alive ∈ {0,1}`. Remove the
    rotate handler.
- `crates/a2fa-cli`, `crates/a2fa-tui`: render single-connection status.
- `auto2fa-mac/Auto2FA/{Models/Host.swift,Views/Components/HostRow.swift,FriendlyText.swift}`:
  single connection indicator.

## Data flow — one heartbeat tick (new)

```
for each active host:
  if in_cooldown:            mark "Cooldown"; continue
  liveness = master_probe(stable_path)          # fork-free unix connect
  match liveness:
    Alive:        failures = 0; note_slot_alive; ensure State=Connected; Healthy
    Inconclusive: (leave failures unchanged); Healthy/keep current
    Dead:         failures += 1
  action = next_action(state, failures, now)
  if action == Restart and not in_flap_backoff:
      if try_begin_start(host):                  # in-flight guard (kept)
          spawn hb-restart worker:               # off the heartbeat thread (kept)
            re-probe; if Alive -> adopt, return   # adopt-before-restart (no kill)
            else: establish master on stable_path (2FA), write back
  if action == AdoptAlive:   mark Connected (no restart, no 2FA)
```

## Error handling / edge cases

- **Master alive but TCP stalled (real network freeze):** probe stays `Alive`
  (process is up); the master's own `ServerAlive` (≤180 s) eventually exits it;
  then probe goes `Dead` and we reconnect. The user's sessions through a frozen
  master were already dead; we do not make it worse by thrashing. (180 s detect
  is conservative-safe against blips; tightening `ServerAliveCountMax` is a
  separate tunable, out of scope.)
- **Socket file lingers after master death:** `connect()` → `ECONNREFUSED` →
  `Dead`; the file is removed on the reconnect path (nothing live to disturb).
- **Reconnect racing a recovering master:** adopt-before-restart re-probes and
  adopts instead of killing/re-authing.
- **Bad secret / unreachable host:** existing login-failure cooldown and flap
  back-off still arm — no change.
- **`connect()` itself hanging:** unix-domain connect does not block on the
  network; we still use a non-blocking socket + tiny `select` timeout →
  `Inconclusive`, never a wedge. (Honors the repo's no-blocking-on-heartbeat
  invariant.)

## Testing

`next_action` and the probe→counter→action logic are **pure** and unit-tested
(extending the existing `next_action_*` tests in `managers.rs`):

- One `Dead` probe on a Ready master → **not** Restart (below threshold).
- `THRESHOLD` consecutive `Dead` → Restart.
- `Inconclusive` does not increment the counter and does not condemn.
- A single `Alive` resets the counter mid-streak.
- State down + probe `Alive` → AdoptAlive (no restart).
- In cooldown / flap-backoff → Skip even at threshold.

`master_probe` liveness mapping is tested against real unix sockets in a
tempdir (listening socket → `Alive`; unlinked-listener socket file →
`Dead`/`ECONNREFUSED`; absent path → `Dead`/`ENOENT`). No network, no 2FA, no
real ssh needed.

Teardown safety: a test asserting `cleanup`/kill is a no-op when a listener is
present on the path.

Whole-system check: `cargo test` green; daemon rebuilt; `package-app.sh`
redeploy; verify the log no longer shows `needs restart (status=Ready,
check=Some(false))` / `killed … ControlMaster` churn, and that hosts stay
`Connected` across induced load.

## Migration / rollout

1. **Boot janitor (runs once on the new daemon):** sweep pre-upgrade leftovers —
   the old `cm-auto2fa-<host>-0/-1` sockets, the `cm-auto2fa-<host>` *symlink*, and
   any leaked `[mux]` masters on those `-N` paths — then establish the single
   stable master. This is the one place a process-kill of an old master is allowed
   (it belongs to the retired scheme). After this, only stable paths exist.
2. **Sequencing (so the critical fix ships and is verified first):**
   - **Stage 1 — de-thrash (highest urgency):** the cheap probe + hysteresis +
     adopt-before-restart + no-kill-live teardown, on the existing structure.
     Rebuild, redeploy, confirm the loop is gone on the live machine.
   - **Stage 2 — structural simplification:** collapse to a single stable master,
     remove symlink/rotation/pool code, simplify State/IPC/UI. Rebuild, redeploy,
     re-verify.
   Each stage is independently testable and revertible.
3. Same daemon version bump so `package-app.sh` installs the new binary (daemon
   restarts once into the new logic).

## Non-goals (YAGNI)

- No change to the 2FA/login mechanism, OTP serialization, Keychain handling, or
  tunnel/port-forward logic.
- No new user-facing settings or keepalive tunables (keep `15×12`).
- Not touching `~/.ssh/config` — the stable path is already what users point at.
- No multi-master/load-balancing; one healthy master per host is the target.

## Security / invariants preserved

- No secret or password ever enters a log/argv/network (unchanged; the probe is a
  local unix connect with no credentials).
- The repo's no-blocking-on-the-heartbeat invariant is *strengthened*: the hot
  path no longer forks `ssh`/`pgrep`; the blocking `ssh -O check`/login stays on
  the off-thread worker with the in-flight guard and hard timeouts.
- `panic = "abort"` remains forbidden (catch_unwind needs unwinding).
