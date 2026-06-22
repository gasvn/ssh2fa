# Rust Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a feature-complete Rust replacement for the Python backend (daemon + CLI + TUI), wire-compatible with the existing IPC protocol, shipping as small static binaries with the daemon embeddable in the Swift app (no CPython).

**Architecture:** One Cargo workspace: `a2fa-core` (library) + three binaries (`ssh2fa-daemon`, `a2fa-cli`, `a2fa-tui`). Synchronous threads; one `Mutex<State>` never held across ssh I/O; system `ssh` + `portable-pty` for ControlMaster + 2FA. Python kept only as a dev-time conformance oracle, deleted at cutover.

**Tech Stack:** Rust (edition 2021), serde/serde_json, portable-pty, keyring, totp-rs, clap, ratatui/crossterm, thiserror/anyhow, log/simplelog, fs2, regex.

**Spec:** `docs/superpowers/specs/2026-06-06-rust-rewrite-design.md`

**Granularity note:** ~5000-line greenfield rewrite. Each task implements one **module directory** to parity with a named Python source; tasks give the file split, public interface (types + signatures), parity reference, key algorithm notes, and the test gate, with representative code for tricky parts. Large module bodies are specified by interface + parity reference + tests rather than pre-written line-for-line (appropriate for greenfield, not hidden TODOs). Build order is dependency-driven; the deliverable is the complete final system.

**Good-Rust file rule:** no monolithic modules. Every non-trivial concern is a **directory module** (`mod.rs` re-exports; logic in focused submodule files, target < ~250 lines each). The layout below is the contract; each task creates/fills the specific files named.

---

## File structure (the contract)

```
auto2fa-rs/
  Cargo.toml                         # workspace
  rustfmt.toml  .gitignore
  crates/
    a2fa-core/
      Cargo.toml
      src/
        lib.rs                       # pub mod proto; model; error; config; creds; totp; ssh; tunnels; engine;
        error.rs                     # thiserror Error + to_errcode()  (small → single file)
        totp.rs                      # TOTP gen + otpauth secret extract (small → single file)
        proto/
          mod.rs                     # re-exports
          method.rs                  # enum Method (28) + as_str/from_str
          event.rs                   # enum Event (3)
          error_code.rs              # enum ErrCode (8)
          wire.rs                    # Request struct + encode_response/error/event
        model/
          mod.rs
          newtype.rs                 # HostName, Port (validated)
          status.rs                  # HostStatus, TunnelStatus enums
          host.rs                    # struct Host (+ snapshot fields)
          tunnel.rs                  # struct Tunnel (+ snapshot fields)
        config/
          mod.rs
          paths.rs                   # config_dir()
          tunnels_store.rs           # load/save tunnels.json (atomic, skip-malformed)
          passwords_store.rs         # passwords.json metadata schema
        creds/
          mod.rs                     # SecretStore trait
          keychain.rs                # KeychainStore (keyring) + FakeStore(cfg test)
          migrate.rs                 # v1→v2 migration
        ssh/
          mod.rs
          control.rs                 # control_path(), master_check/exit (ssh -O)
          master.rs                  # pool-of-2 lifecycle, rotation, symlink, cooldown/backoff
          pty_auth.rs                # portable-pty spawn + expect(password/OTP) loop
          failure.rs                 # failure_reason(output)
        tunnels/
          mod.rs
          forward.rs                 # start/stop ssh -L
          probe.rs                   # port_available(), probe_port_ready()
          discovery.rs               # squeue parse + discover_nodes()
          post_connect.rs            # hook runner (threaded, no double-spawn)
          uptime.rs                  # accumulate/live uptime
        engine/
          mod.rs                     # State struct + Mutex discipline doc
          change_key.rs              # host/tunnel stable-field change keys (exclude uptime)
          tick.rs                    # 0.5s poll loop, emit-on-change
          recovery.rs                # wake_recover, reset_all
          schedule.rs                # cooldown/backoff/heartbeat scheduling
    ssh2fa-daemon/
      Cargo.toml
      src/
        main.rs                      # startup: lock, load, spawn workers+tick, serve
        singleton.rs                 # fs2 flock single-instance
        server.rs                    # unix listener, per-conn thread, line framing
        subscribers.rs               # event fan-out to subscribed writers
        dispatch.rs                  # Method -> handler routing
        handlers/
          mod.rs
          hosts.rs                   # list_hosts, host_toggle/add/rotate/mount/test_credentials
          tunnels.rs                 # tunnel_* (add/remove/start/stop/toggle/set_*/rename/batch)
          system.rs                  # ping, discover_nodes, port_suggest, wake_recover, reset_all, log_tail, subscribe_events, tunnel_events
    a2fa-cli/
      Cargo.toml
      src/
        main.rs                      # no-arg -> launch TUI; else parse+dispatch
        cli.rs                       # clap command tree -> (method, params)
        client.rs                    # socket rpc (timeout, clean errors)
    a2fa-tui/
      Cargo.toml
      src/
        main.rs                      # terminal setup + run loop
        client.rs                    # socket subscribe + request
        app.rs                       # app state / reducer (unit-testable)
        views/
          mod.rs
          hosts.rs  tunnels.rs  logs.rs  sheets.rs   # render fns + keybindings
  tests/
    conformance.rs                   # Rust daemon vs Python oracle
```

---

## Task 1: Workspace scaffold + module skeletons

**Files:** Create the workspace `Cargo.toml`, `rustfmt.toml`, `.gitignore`; all four crate `Cargo.toml`s; and **every `mod.rs` / module file from the tree above as a stub** (each `mod.rs` declares its submodules; each leaf file `// implemented in Task N`). `lib.rs` declares the 9 top modules.

- [ ] **Step 1:** Workspace `Cargo.toml` (members = the 4 crates; `[workspace.dependencies]` for serde, serde_json, thiserror, anyhow, log, simplelog, regex; `[workspace.package] edition="2021"`). `.gitignore` = `/target`. `rustfmt.toml` = `max_width=100`.
- [ ] **Step 2:** `a2fa-core` `Cargo.toml` (serde, serde_json, thiserror, log, regex; dev-deps tempfile). Create `lib.rs` + `error.rs`,`totp.rs` stubs + every directory module's `mod.rs` declaring its leaf submodules, and each leaf file as a stub comment. Mirror the tree exactly.
- [ ] **Step 3:** The three bin crates' `Cargo.toml` (each `a2fa-core = { path = "../a2fa-core" }` + `anyhow`; daemon adds `fs2`; cli adds `clap` w/ derive; tui adds `ratatui`,`crossterm`) and their `main.rs`/submodule stubs per the tree.
- [ ] **Step 4:** Run `cd auto2fa-rs && cargo build` → compiles (empty-module warnings OK). Run `cargo fmt --check` and `cargo clippy` → clean enough.
- [ ] **Step 5:** Commit `git add auto2fa-rs && git commit -m "feat(rust): workspace + module-tree scaffold"`

---

## Task 2: proto/ — IPC protocol types (wire-compatible)

**Files:** `crates/a2fa-core/src/proto/{mod.rs,method.rs,event.rs,error_code.rs,wire.rs}`. **Parity:** `auto2fa/ipc.py`. **Tests:** in `wire.rs`/`method.rs` `#[cfg(test)]`.

- [ ] **Step 1: Failing tests**
```rust
// method.rs tests
#[test] fn method_strings_match_python() {
    assert_eq!(Method::ListHosts.as_str(), "list_hosts");
    assert_eq!(Method::TunnelSetJumpCandidates.as_str(), "tunnel_set_jump_candidates");
    assert_eq!(Method::from_str("host_add"), Some(Method::HostAdd));
}
// wire.rs tests
#[test] fn request_decodes_response_encodes() {
    let r: Request = serde_json::from_str(r#"{"id":"1","method":"list_hosts","params":{}}"#).unwrap();
    assert_eq!(r.method, "list_hosts");
    let line = encode_response("1", serde_json::json!({"ok":true}));
    assert!(line.ends_with('\n') && line.contains("\"result\""));
}
// error_code.rs tests
#[test] fn errcode_strings() { assert_eq!(ErrCode::PortInUse.as_str(), "port_in_use"); }
```
- [ ] **Step 2:** `cargo test -p a2fa-core proto` → FAIL.
- [ ] **Step 3: Implement** `method.rs` (`enum Method` all 28 + `as_str`/`from_str`), `event.rs` (`enum Event` 3 + `as_str`), `error_code.rs` (`enum ErrCode` 8 + `as_str`), `wire.rs` (`struct Request{id,method,params:Value(#[serde(default)])}` + `encode_response/encode_error/encode_event` each `\n`-terminated). `mod.rs` re-exports all.
- [ ] **Step 4:** `cargo test -p a2fa-core proto` → PASS.
- [ ] **Step 5:** Commit `feat(rust): proto module (wire-compatible IPC types)`

---

## Task 3: model/ — newtypes, status enums, Host/Tunnel

**Files:** `src/model/{mod.rs,newtype.rs,status.rs,host.rs,tunnel.rs}`. **Parity:** `tunnels.py` TunnelState, `daemon.py` snapshots.

- [ ] **Step 1: Failing tests** (in `newtype.rs`)
```rust
#[test] fn hostname_rejects_traversal() {
    assert!(HostName::new("k6").is_ok());
    for bad in ["../x","a/b","..",".","" ,"a..b","/etc"] { assert!(HostName::new(bad).is_err(), "{bad}"); }
}
#[test] fn port_range() { assert!(Port::new(8090).is_ok()); assert!(Port::new(80).is_err()); assert!(Port::new(70000).is_err()); }
```
- [ ] **Step 2:** `cargo test -p a2fa-core model` → FAIL.
- [ ] **Step 3: Implement** `newtype.rs` (`HostName`,`Port` validated, `Display`+serde passthrough), `status.rs` (`HostStatus`,`TunnelStatus` with serde rename to existing lowercase strings), `host.rs`/`tunnel.rs` (structs with the exact persisted+snapshot field names from the spec). `mod.rs` re-exports.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(rust): model (validated newtypes + Host/Tunnel)`

---

## Task 4: error.rs — domain error → IPC code

**Files:** `src/error.rs`. **Parity:** `ipc.py` ErrCode.
- [ ] **Step 1: Failing test**
```rust
#[test] fn maps_codes() {
    use crate::proto::ErrCode;
    assert_eq!(Error::NotFound("x".into()).to_errcode(), ErrCode::NotFound);
    assert_eq!(Error::PortInUse(8090).to_errcode(), ErrCode::PortInUse);
    assert_eq!(Error::Discovery("squeue".into()).to_errcode(), ErrCode::DiscoveryFailed);
}
```
- [ ] **Step 2:** FAIL. **Step 3:** `#[derive(thiserror::Error)] enum Error { NotFound, BadParams, PortInUse(u16), Duplicate, Discovery, Io(#[from]), Internal }` + `to_errcode()`; `pub type Result<T>`. **Step 4:** PASS. **Step 5:** Commit `feat(rust): error type + IPC code mapping`.

---

## Task 5: config/ — config_dir + persistence

**Files:** `src/config/{mod.rs,paths.rs,tunnels_store.rs,passwords_store.rs}`. **Parity:** `credentials.py` (config_dir, passwords schema), `tunnels.py` (load/save, PERSISTED_FIELDS).
- [ ] **Step 1: Failing tests** — `paths.rs`: `config_dir()` returns `~/.ssh` when `SSH_CONFIG_PATH` unset or non-existent. `tunnels_store.rs`: round-trip a tunnel; a tunnel missing `local_port` is skipped (use `tempfile::tempdir`).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3: Implement** `paths.rs::config_dir()` (honor `SSH_CONFIG_PATH` only if existing dir else `~/.ssh`); `tunnels_store.rs::{load_tunnels,save_tunnels}` (atomic tmp+fsync+rename; skip malformed); `passwords_store.rs::{load_meta,save_meta}` (v2 schema: per-host autoConnect + Keychain-backed fields).
- [ ] **Step 4:** PASS. **Step 5:** Commit `feat(rust): config_dir + atomic persistence`.

---

## Task 6: totp.rs — TOTP

**Files:** `src/totp.rs` (+ `totp-rs` dep). **Parity:** `backend.py` OTP + otpauth parse.
- [ ] **Step 1: Failing tests** — `extract_secret("otpauth://...secret=JBSWY3DPEHPK3PXP...")=="JBSWY3DPEHPK3PXP"`; `totp_now(secret)` is 6 digits.
- [ ] **Step 2:** FAIL. **Step 3:** `extract_secret(url)->Result<String>`, `totp_now(secret)`, `totp_at(secret, ts)`. **Step 4:** PASS. **Step 5:** Commit `feat(rust): TOTP + otpauth extraction`.

---

## Task 7: creds/ — Keychain + migration

**Files:** `src/creds/{mod.rs,keychain.rs,migrate.rs}` (+ `keyring` dep). **Parity:** `credentials.py`.
- [ ] **Step 1: Failing test** (in `mod.rs` using a `FakeStore`): a two-write store-credentials rolls back the first write if the second fails.
- [ ] **Step 2:** FAIL. **Step 3:** `mod.rs`: `trait SecretStore{get/set/delete}` + `store_credentials`(two-write atomic, `{host}.password`/`{host}.otpauth` accounts); `keychain.rs`: `KeychainStore` (keyring, existing `KEYCHAIN_SERVICE`) + `FakeStore`(cfg test); `migrate.rs`: v1→v2. **Step 4:** PASS. **Step 5:** Commit `feat(rust): Keychain store + migration`.

---

## Task 8: ssh/ — ControlMaster + pty 2FA (HIGH RISK — prototype first)

**Files:** `src/ssh/{mod.rs,control.rs,master.rs,pty_auth.rs,failure.rs}` (+ `portable-pty`). **Parity:** `backend.py`.
- [ ] **Step 1: Prototype FIRST** — `examples/ssh_login.rs`: spawn `ssh -M -S <tmp controlpath> user@host` via `portable-pty`, expect-loop (read→`assword:`→write pw; read→OTP prompt→write `totp_now`), then `ssh -O check`. Run manually against a real cluster host until it connects; record the working prompt regexes. Do not proceed until it connects.
- [ ] **Step 2: Unit tests** — `failure.rs`: `failure_reason("...Permission denied (publickey).")=="Permission denied"`; `control.rs`: `control_path("k6",0)` contains "k6".
- [ ] **Step 3:** FAIL.
- [ ] **Step 4: Implement** `control.rs` (control_path, master_check/exit via `ssh -O`), `pty_auth.rs` (the prototyped pty+expect loop, takes a `submit_otp` closure the caller guards with the per-secret lock), `master.rs` (pool-of-2, rotation+symlink, cooldown after N fails, probe back-off, `is_master_ready`), `failure.rs`. `mod.rs` re-exports.
- [ ] **Step 5:** unit tests PASS; re-run prototype → connects. **Step 6:** Commit `feat(rust): ssh ControlMaster + pty 2FA auth`.

---

## Task 9: tunnels/ — forwards, probe, discovery, hooks

**Files:** `src/tunnels/{mod.rs,forward.rs,probe.rs,discovery.rs,post_connect.rs,uptime.rs}`. **Parity:** `tunnels.py`.
- [ ] **Step 1: Failing tests** — `discovery.rs`: `parse_squeue("123|gpu|run|RUNNING|01:00:00|holygpu01\nbad\n")` → 1 job, node `holygpu01`; `probe.rs`: bind a port then `port_available()` is false.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3: Implement** `discovery.rs` (`parse_squeue`, `discover_nodes(jump)->Result` → `Error::Discovery` on fail), `probe.rs` (`port_available`, `probe_port_ready`), `forward.rs` (`start_tunnel` spawns `ssh -N -J jump -L...`, probes, **terminates child on ANY probe error**, persists wants_alive; `stop_tunnel`), `post_connect.rs` (threaded hook, no double-spawn), `uptime.rs` (accumulate/live). `mod.rs` re-exports.
- [ ] **Step 4:** PASS. **Step 5:** Commit `feat(rust): tunnels (forward/probe/discovery/hooks)`.

---

## Task 10: engine/ — State, tick, change-key, recovery

**Files:** `src/engine/{mod.rs,change_key.rs,tick.rs,recovery.rs,schedule.rs}`. **Parity:** `daemon.py` poll loop + change keys, `backend.py` heartbeat.
- [ ] **Step 1: Failing test** (`change_key.rs`) — `tunnel_change_key` ignores `total_uptime_sec` (uptime-only diff → equal keys → NO event); a status change → different keys.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3: Implement** `mod.rs` (`struct State{hosts,tunnels,subscribers,...}` + doc: never hold `Mutex<State>` across ssh I/O), `change_key.rs` (stable-field keys per spec; exclude uptime; host excludes last_msg), `tick.rs` (0.5s loop: maintenance off-lock → snapshot → emit on stable-key change), `recovery.rs` (`wake_recover`,`reset_all`; snapshot dict before iterating), `schedule.rs` (cooldown/backoff/heartbeat).
- [ ] **Step 4:** PASS. **Step 5:** Commit `feat(rust): engine (state + tick + change detection + recovery)`.

---

## Task 11: ssh2fa-daemon — server + dispatch + handlers

**Files:** `crates/ssh2fa-daemon/src/{main.rs,singleton.rs,server.rs,subscribers.rs,dispatch.rs,handlers/{mod.rs,hosts.rs,tunnels.rs,system.rs}}` (+ `fs2`). **Parity:** `daemon.py`.
- [ ] **Step 1: Failing tests** (`tests/dispatch.rs`) — invalid UTF-8 line → `invalid_request`; unknown method → `unknown_method`; `list_hosts` on a seeded State → JSON array.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3: Implement** `singleton.rs` (`fs2` exclusive lock on `~/.ssh2fa/lock`), `server.rs` (unix listener, remove stale sock, chmod 0600, thread/conn, line framing, invalid-bytes→error), `subscribers.rs` (writer registry + fan-out), `dispatch.rs` (`Method`→handler), `handlers/*` (the 28 methods grouped: hosts/tunnels/system), `main.rs` (lock→load config/creds→spawn host workers + tick thread→serve; log→`/tmp/ssh2fa_daemon.log`). Binary name `ssh2fa-daemon`.
- [ ] **Step 4:** `cargo test -p a2fa-daemon` PASS; manual `ping` over the socket. **Step 5:** Commit `feat(rust): daemon server + 28 handlers`.

---

## Task 12: Conformance harness (vs Python oracle)

**Files:** `auto2fa-rs/tests/conformance.rs`.
- [ ] **Step 1:** Drive read-only requests (`ping,list_hosts,list_tunnels,port_suggest,log_tail`) against both the Python daemon socket and the Rust daemon (temp socket via env override); assert structural JSON equality (normalize uptime/timestamps).
- [ ] **Step 2:** Run; fix proto/handler shape mismatches until green.
- [ ] **Step 3:** Commit `test(rust): conformance harness vs Python oracle`.

---

## Task 13: a2fa-cli — clap subcommands + client

**Files:** `crates/a2fa-cli/src/{main.rs,cli.rs,client.rs}` (+ `clap`). **Parity:** `cli.py`.
- [ ] **Step 1: Failing test** (`cli.rs`) — parsing `["node","jup","compute-01"]` builds method `tunnel_set_node`, params `{name,node}`.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3: Implement** `cli.rs` (clap tree mirroring `cli.py`, de-personalized help), `client.rs` (`rpc(method,params)` socket, 30s timeout, clean errors on connect/timeout/malformed), `main.rs` (no-arg → launch TUI; else parse→rpc→print).
- [ ] **Step 4:** PASS; manual `auto2fa list`. **Step 5:** Commit `feat(rust): CLI`.

---

## Task 14: a2fa-tui — ratatui UI

**Files:** `crates/a2fa-tui/src/{main.rs,client.rs,app.rs,views/{mod.rs,hosts.rs,tunnels.rs,logs.rs,sheets.rs}}` (+ `ratatui`,`crossterm`). **Parity:** `main.py`.
- [ ] **Step 1: Unit-test the reducer** (`app.rs`) — status→color mapping; list filtering. Run → FAIL.
- [ ] **Step 2: Implement** `app.rs` (view-model + reducer, pure-testable), `client.rs` (subscribe+rpc), `views/*` (hosts/tunnels tables, keybindings start/stop/toggle, node picker, add-host/new-tunnel sheets, log viewer; **static/low-frequency indicators only**), `main.rs` (terminal setup + run loop).
- [ ] **Step 3:** reducer tests PASS; manual `auto2fa-tui` against the daemon. **Step 4:** Commit `feat(rust): ratatui TUI`.

---

## Task 15: Build & distribution (universal + embed)

**Files:** `auto2fa-rs/build-release.sh`; modify `auto2fa-mac/build.sh`, `auto2fa-mac/project.yml`.
- [ ] **Step 1:** `build-release.sh`: build both `aarch64`/`x86_64-apple-darwin`, `lipo` the 3 binaries into `auto2fa-rs/dist/`.
- [ ] **Step 2:** `auto2fa-mac/build.sh` embeds `dist/ssh2fa-daemon` at `Contents/Resources/daemon/ssh2fa-daemon` (replaces the PyInstaller embed) + keeps the SMAppService agent plist (BundleProgram→it); re-sign ad-hoc.
- [ ] **Step 3:** Verify: build app; clean-env smoke the embedded daemon (`env -i … ssh2fa-daemon` → binds/flock-exits, no dyld errors).
- [ ] **Step 4:** Commit `feat(rust): universal build + embed daemon in app`.

---

## Task 16: Cutover — switch to Rust, delete Python

**Files:** delete `auto2fa/`, `packaging/`, Python `tests/`, `setup.py`, `requirements.txt`, `install.py`; update `auto2fa-mac` install path + `README.md`.
- [ ] **Step 1:** Full conformance harness + Swift app e2e (connect host, start tunnel, mount) green against the Rust daemon.
- [ ] **Step 2:** `cd auto2fa-rs && cargo test` all pass; `cargo clippy -- -D warnings` clean; `cargo fmt --check`.
- [ ] **Step 3:** Point installer + Swift embed at the Rust binaries; update README to Rust build/run.
- [ ] **Step 4:** Delete Python package + packaging + Python tests; `rg -n "python|\.py\b" README.md` → clean.
- [ ] **Step 5:** Final manual e2e: restart Rust daemon via the app; hosts/tunnels serve; terminal `ssh host` instant (ControlMaster).
- [ ] **Step 6:** Commit `feat(rust): cutover to Rust backend; remove Python`.

---

## Self-Review

**Spec coverage:** workspace+tree (T1); proto/28 methods (T2,T11); newtypes/model (T3); error→code (T4); config_dir+persistence (T5); totp (T6); keychain+migration (T7); ssh ControlMaster+pty 2FA (T8); tunnels+squeue+probe+hooks (T9); engine+change-key(no uptime storm)+wake/reset (T10); daemon server+flock+0600+handlers (T11); conformance oracle (T12); CLI (T13); TUI (T14); universal build+embed (T15); cutover+delete Python+de-personalized (T16). Feature-parity items 1-13 map to T2-T14.

**Good-Rust file decomposition:** every non-trivial module is a directory with focused leaf files (proto/, model/, config/, creds/, ssh/, tunnels/, engine/, daemon handlers/, tui views/) — no monolithic `.rs`. Locked in T1's tree and each task fills its named files.

**Placeholder scan:** tricky/small units have concrete code+tests; large bodies specified by interface+parity+tests (noted in granularity rule). ssh is prototype-first.

**Type/name consistency:** `Method`/`Event`/`ErrCode` (proto, T2) consumed by daemon (T11)/cli (T13); `HostName`/`Port` (T3) in config (T5)/tunnels (T9)/handlers; `Error::to_errcode` (T4) across; `tunnel_change_key` excludes `total_uptime_sec` (T10 test+spec); `control_path`/`master_check` (T8) used by engine/handlers; binary `ssh2fa-daemon` + embed path `Contents/Resources/daemon/ssh2fa-daemon` consistent (T11,T15) with P1 agent plist BundleProgram.
```
