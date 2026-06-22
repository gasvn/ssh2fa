# Zero-Config Setup — Phase 1 (zero-config login core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A freshly-installed user with an empty `~/.ssh/config` can add a host by typing name/address/username + password + 2FA and log in — the daemon resolves the connection from an app-owned ssh config via `ssh -F`, never editing the user's `~/.ssh/config`.

**Architecture:** App collects connection fields → writes a sidecar JSON (alias→{hostName,user,port}) → generates a managed `~/.ssh/ssh2fa.conf` (Host blocks with HostName/User/Port + ControlMaster lines) + a daemon wrapper `~/.ssh/ssh2fa-daemon.conf` (`Include`s the managed file + the user's config). The Rust daemon adds `-F <wrapper>` to its ssh invocations, falling back to today's behavior when the wrapper is absent.

**Tech Stack:** Rust (a2fa-core, a2fa-daemon; `cargo test`), Swift/SwiftUI (auto2fa-mac; xcodebuild + headless XCTest bundle).

**Reference spec:** `docs/superpowers/specs/2026-06-21-zero-config-setup-and-self-healing-design.md` (this plan implements **Phase 1** from §8; Phase 2 = self-heal reconcile + auto-Include is a separate later plan.)

**Scope note (what Phase 1 does NOT touch):** the `WarmReuseConsent` terminal-reuse Include stays as-is (opt-in) — Phase 1 makes login work via the daemon `-F` regardless of whether the user enabled the terminal Include. Dropping that consent alert is Phase 2.

Rust working dir for `cargo`: `/Users/shgao/logs/auto2fa_dev/auto2fa-rs`. Swift project: `/Users/shgao/logs/auto2fa_dev/auto2fa-mac`.

---

## File structure

**Rust (`a2fa-core`)**
- `crates/a2fa-core/src/config/paths.rs` — add `ssh_dir()`, `daemon_ssh_config_path()`, `managed_config_args()`.
- `crates/a2fa-core/src/ssh/master.rs` — `-F` into the master login argv.
- `crates/a2fa-core/src/ssh/control.rs` — `-F` into `run_ssh_g`.
- `crates/a2fa-core/src/tunnels/forward.rs` — `-F` into `build_forward_argv` + `build_direct_argv`.

**Rust (`a2fa-daemon`)**
- `crates/a2fa-daemon/src/handlers/hosts.rs` — `-F` into `test_login`.

**Swift (`auto2fa-mac`)**
- `Auto2FA/ManagedHostStore.swift` (new) — sidecar JSON store (`ManagedHostConn` + read/write).
- `Auto2FA/SSHConfigManager.swift` — generator takes connection fields; add `daemonWrapper(...)` + `hostNameConflict(...)` + `sanitizeAlias(...)`.
- `Auto2FA/AppState.swift` — `addManagedHost(...)`; ungate + extend `syncSSHConfigIfEnabled` → always write managed conf + wrapper.
- `Auto2FA/Views/AddHostSheet.swift` — collect name/address/user/port; write config before test-login.
- `Auto2FA/BackendClient.swift` / `AppState.addHost` — pass through unchanged (alias is the sanitized name).
- `Auto2FATests/ManagedHostStoreTests.swift`, `SSHConfigGenTests.swift` (new) + `project.yml` test-target sources.

---

## Task 1: Rust path helpers + `managed_config_args()`

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/config/paths.rs`

- [ ] **Step 1: Write the failing test**

Append to `paths.rs` (create a `#[cfg(test)] mod tests` block if none exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_dir_is_home_dot_ssh() {
        // ssh_dir is $HOME/.ssh (HOME is always set in the test env).
        let home = std::env::var("HOME").unwrap();
        assert_eq!(ssh_dir(), std::path::PathBuf::from(home).join(".ssh"));
    }

    #[test]
    fn daemon_ssh_config_path_is_in_ssh_dir() {
        assert_eq!(daemon_ssh_config_path(), ssh_dir().join("ssh2fa-daemon.conf"));
    }

    #[test]
    fn managed_config_args_empty_when_wrapper_absent() {
        // A path that does not exist → no -F (backward-compat fallback).
        let missing = std::path::PathBuf::from("/no/such/ssh2fa-daemon.conf");
        assert!(managed_config_args_for(&missing).is_empty());
    }

    #[test]
    fn managed_config_args_has_dash_f_when_wrapper_present() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ssh2fa-daemon.conf");
        std::fs::write(&p, "Include ~/.ssh/config\n").unwrap();
        let args = managed_config_args_for(&p);
        assert_eq!(args, vec!["-F".to_string(), p.to_string_lossy().into_owned()]);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-core --lib config::paths 2>&1 | tail -15`
Expected: FAIL — `cannot find function ssh_dir` / `daemon_ssh_config_path` / `managed_config_args_for`.

- [ ] **Step 3: Implement the helpers**

In `paths.rs`, after `config_dir()` (before the `expand_tilde` fn), add:

```rust
/// The user's `~/.ssh` directory — where the ControlPath sockets, the managed
/// `ssh2fa.conf`, and the daemon wrapper live. Distinct from [`config_dir`]
/// (which holds passwords.json and may be elsewhere via `SSH_CONFIG_PATH`).
pub fn ssh_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    std::path::PathBuf::from(home).join(".ssh")
}

/// The app-owned ssh config the daemon reads via `ssh -F`. The app writes it
/// (Includes the managed hosts file + the user's `~/.ssh/config`); the daemon
/// only reads it. Co-located with `~/.ssh` so a relative `Include ssh2fa.conf`
/// resolves correctly.
pub fn daemon_ssh_config_path() -> std::path::PathBuf {
    ssh_dir().join("ssh2fa-daemon.conf")
}

/// ssh args that point ssh at the app-managed config: `["-F", <wrapper>]` when
/// the wrapper exists, else EMPTY — so a daemon running before the app has
/// written the wrapper (or an older install) falls back to resolving from the
/// user's own `~/.ssh/config` exactly as before. Never hard-fails on absence.
pub fn managed_config_args() -> Vec<String> {
    managed_config_args_for(&daemon_ssh_config_path())
}

/// Testable core of [`managed_config_args`].
pub fn managed_config_args_for(wrapper: &std::path::Path) -> Vec<String> {
    if wrapper.is_file() {
        vec!["-F".to_owned(), wrapper.to_string_lossy().into_owned()]
    } else {
        Vec::new()
    }
}
```

Ensure `tempfile` is a dev-dependency of `a2fa-core` (it is — `tunnels_store.rs` tests use it).

- [ ] **Step 4: Run to verify it passes**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-core --lib config::paths 2>&1 | tail -15`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-core/src/config/paths.rs
git commit -m "feat(ssh): app-managed ssh config path helpers + -F arg builder (with absent-wrapper fallback)"
```

---

## Task 2: `-F` into the master login argv (master.rs)

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/ssh/master.rs`

The master login argv is built around line 407 (`let argv: Vec<String> = vec![ "-E", log_file, ... , state.host.clone() ];`) and passed to `run_login`. Prepend the managed-config args so ssh resolves the host from the wrapper.

- [ ] **Step 1: Make the change**

In `master.rs`, replace the `let argv: Vec<String> = vec![ … ];` block (the one starting with `"-E".into(), log_file,` and ending with `state.host.clone(),`) so it prepends the `-F` args:

```rust
    let mut argv: Vec<String> = crate::config::paths::managed_config_args();
    argv.extend([
        "-E".into(),      log_file,
        "-o".into(),      "StrictHostKeyChecking=no".into(),
        "-o".into(),      "UserKnownHostsFile=/dev/null".into(),
        "-o".into(),      "ServerAliveInterval=15".into(),
        "-o".into(),      "ServerAliveCountMax=12".into(),
        "-o".into(),      "ConnectTimeout=10".into(),
        "-o".into(),      "ControlMaster=auto".into(),
        "-o".into(),      format!("ControlPath={control_path_str}"),
        "-o".into(),      "ControlPersist=yes".into(),
        state.host.clone(),
    ]);
```

(`-F` must come before the host; placing the whole managed-config args first satisfies that. The explicit `-o ControlPath=` still wins over any ControlPath in the config because command-line `-o` beats config-file values.)

- [ ] **Step 2: Build + run existing master tests**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo build -p a2fa-core 2>&1 | tail -3 && cargo test -p a2fa-core --lib ssh::master 2>&1 | tail -8`
Expected: builds; existing master tests still pass (no behavior change when the wrapper is absent, which is the case in tests).

- [ ] **Step 3: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-core/src/ssh/master.rs
git commit -m "feat(ssh): master login resolves via the app-managed ssh config (-F)"
```

---

## Task 3: `-F` into `run_ssh_g` (control.rs)

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/ssh/control.rs`

`run_ssh_g` (around line 99) runs `ssh -G <host>` to discover the ControlPath. It must resolve from the same managed config or it would read a different (or missing) ControlPath than the master uses.

- [ ] **Step 1: Make the change**

In `control.rs`, in `run_ssh_g`, replace the `Command::new("ssh").args(["-G", &host_owned])` invocation (around line 110-114) with one that prepends the managed-config args:

```rust
            let mut g_args: Vec<String> = crate::config::paths::managed_config_args();
            g_args.push("-G".into());
            g_args.push(host_owned.clone());
            let child = Command::new("ssh")
                .args(&g_args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn();
```

(`host_owned` is already a `String` moved into the thread; `.clone()` keeps it usable for the args vec. If the borrow checker complains about a later use of `host_owned`, use `&host_owned` in a `push(host_owned)` at the end instead.)

- [ ] **Step 2: Build + run control tests**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo build -p a2fa-core 2>&1 | tail -3 && cargo test -p a2fa-core --lib ssh::control 2>&1 | tail -8`
Expected: builds; existing control tests pass.

- [ ] **Step 3: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-core/src/ssh/control.rs
git commit -m "feat(ssh): ssh -G ControlPath discovery resolves via the app-managed config (-F)"
```

---

## Task 4: `-F` into the forward argv builders (forward.rs)

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/tunnels/forward.rs`

`build_forward_argv` (compute) and `build_direct_argv` (direct) build the `ssh -N …` argv. Tunnels resolve the jump/host from config too, so they need `-F`.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `forward.rs`:

```rust
    #[test]
    fn forward_argv_starts_with_managed_config_when_present() {
        // build_forward_argv_with lets the test inject the -F prefix
        // deterministically (the real builder consults the live wrapper path).
        let argv = build_forward_argv_with(
            &["-F".into(), "/x/ssh2fa-daemon.conf".into()],
            "jump", "user", "node", 8080, 8888,
        );
        assert_eq!(argv[0], "-F");
        assert_eq!(argv[1], "/x/ssh2fa-daemon.conf");
        // -N still present and host still last.
        assert!(argv.contains(&"-N".to_string()));
        assert_eq!(argv.last().unwrap(), "user@node");
    }

    #[test]
    fn direct_argv_starts_with_managed_config_when_present() {
        let argv = build_direct_argv_with(
            &["-F".into(), "/x/ssh2fa-daemon.conf".into()],
            "loginhost", 9000, 9000,
        );
        assert_eq!(argv[0], "-F");
        assert_eq!(argv.last().unwrap(), "loginhost");
        assert!(!argv.contains(&"-J".to_string()));
    }

    #[test]
    fn forward_argv_no_prefix_is_todays_shape() {
        let argv = build_forward_argv_with(&[], "jump", "user", "node", 8080, 8888);
        assert_eq!(argv[0], "-N"); // unchanged when no wrapper
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-core --lib tunnels::forward 2>&1 | tail -15`
Expected: FAIL — `cannot find function build_forward_argv_with` / `build_direct_argv_with`.

- [ ] **Step 3: Refactor the builders to take an injectable prefix**

In `forward.rs`, change `build_forward_argv` to delegate to a `_with` variant that takes a prefix, and have the public fn read the live wrapper path:

```rust
pub fn build_forward_argv(
    jump: &str, user: &str, node: &str, local_port: u16, remote_port: u16,
) -> Vec<String> {
    build_forward_argv_with(
        &crate::config::paths::managed_config_args(),
        jump, user, node, local_port, remote_port,
    )
}

/// Testable core: `prefix` is prepended verbatim (the `["-F", <path>]` args, or
/// empty). Keeps the wrapper-path lookup out of the pure argv assembly.
pub fn build_forward_argv_with(
    prefix: &[String],
    jump: &str, user: &str, node: &str, local_port: u16, remote_port: u16,
) -> Vec<String> {
    let mut args: Vec<String> = prefix.to_vec();
    args.push("-N".into());
    args.push("-J".into());
    args.push(jump.to_string());
    args.push("-L".into());
    args.push(format!("{local_port}:localhost:{remote_port}"));
    for (key, val) in SSH_OPTS {
        args.push("-o".into());
        args.push(format!("{key}={val}"));
    }
    args.push(format!("{user}@{node}"));
    args
}
```

And the same shape for direct:

```rust
pub fn build_direct_argv(host: &str, local_port: u16, remote_port: u16) -> Vec<String> {
    build_direct_argv_with(&crate::config::paths::managed_config_args(), host, local_port, remote_port)
}

pub fn build_direct_argv_with(
    prefix: &[String], host: &str, local_port: u16, remote_port: u16,
) -> Vec<String> {
    let mut args: Vec<String> = prefix.to_vec();
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

NOTE: the EXISTING forward tests assert `argv[0] == "-N"` and structure. Those call `build_forward_argv`/`build_direct_argv`, whose prefix is `managed_config_args()` = empty in the test env (no wrapper), so `argv[0]` stays `-N` and they keep passing. Do not change those tests.

- [ ] **Step 4: Run to verify all forward tests pass**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test -p a2fa-core --lib tunnels::forward 2>&1 | tail -15`
Expected: all pass (existing + 3 new).

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-core/src/tunnels/forward.rs
git commit -m "feat(tunnels): forward + direct argv resolve via the app-managed config (-F)"
```

---

## Task 5: `-F` into `test_login` (hosts.rs)

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/handlers/hosts.rs`

`test_login` (around line 797) builds an ssh argv for the credential test. A guided host is NOT in the user's `~/.ssh/config`, so the test must resolve via the wrapper or it fails.

- [ ] **Step 1: Read the function to find its argv**

Read `auto2fa-rs/crates/a2fa-daemon/src/handlers/hosts.rs` around lines 797-860 to find where `test_login` assembles its ssh argv (it builds a `Vec<String>` similar to master.rs and calls `run_login`, OR calls a core helper). Identify the argv vector.

- [ ] **Step 2: Prepend the managed-config args**

Wherever `test_login` builds its argv `Vec<String>` (the one passed to `run_login`/the pty), change it to start from `a2fa_core::config::paths::managed_config_args()` and `.extend(...)` the rest — mirroring Task 2. If it currently is `let argv = vec!["-o", ..., host]`, make it:

```rust
    let mut argv: Vec<String> = a2fa_core::config::paths::managed_config_args();
    argv.extend([ /* the existing -o options … */ , host.clone() ]);
```

Match the EXACT existing options in the function (do not invent options — copy what's there, only prepend the managed-config args and keep the host last).

- [ ] **Step 3: Build + run host handler tests**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo build -p a2fa-daemon 2>&1 | tail -3 && cargo test -p a2fa-daemon --lib handlers::hosts 2>&1 | tail -8`
Expected: builds; existing host tests pass.

- [ ] **Step 4: Full Rust suite**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test --workspace -- --test-threads=1 2>&1 | grep -cE "test result: FAILED" | xargs echo "FAILED suites:"`
Expected: `FAILED suites: 0`.

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-rs/crates/a2fa-daemon/src/handlers/hosts.rs
git commit -m "feat(hosts): credential test-login resolves via the app-managed config (-F)"
```

---

## Task 6: Swift sidecar store (`ManagedHostStore`)

**Files:**
- Create: `auto2fa-mac/Auto2FA/ManagedHostStore.swift`
- Create: `auto2fa-mac/Auto2FATests/ManagedHostStoreTests.swift`
- Modify: `auto2fa-mac/project.yml` (add both to the test target)

- [ ] **Step 1: Write the store (pure, Foundation-only)**

Create `auto2fa-mac/Auto2FA/ManagedHostStore.swift`:

```swift
import Foundation

/// One guided-host connection record. The app is the source of truth for these;
/// the daemon never sees them directly — they are rendered into ssh2fa.conf.
struct ManagedHostConn: Codable, Equatable {
    var alias: String
    var hostName: String
    var user: String
    var port: Int
}

/// Tiny JSON sidecar (alias → connection fields) at ~/.ssh2fa/managed_hosts.json.
/// Pure I/O over an injectable file URL so it unit-tests headlessly.
enum ManagedHostStore {
    /// Decode the sidecar; missing/garbage file → empty (never throws to caller).
    static func load(from url: URL) -> [ManagedHostConn] {
        guard let data = try? Data(contentsOf: url) else { return [] }
        return (try? JSONDecoder().decode([ManagedHostConn].self, from: data)) ?? []
    }

    /// Upsert one record by alias and write back atomically. Returns the new list.
    @discardableResult
    static func upsert(_ conn: ManagedHostConn, in url: URL) throws -> [ManagedHostConn] {
        var list = load(from: url).filter { $0.alias != conn.alias }
        list.append(conn)
        list.sort { $0.alias < $1.alias }
        try write(list, to: url)
        return list
    }

    /// Remove the record for `alias` (no-op if absent). Returns the new list.
    @discardableResult
    static func remove(alias: String, in url: URL) throws -> [ManagedHostConn] {
        let list = load(from: url).filter { $0.alias != alias }
        try write(list, to: url)
        return list
    }

    private static func write(_ list: [ManagedHostConn], to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        try enc.encode(list).write(to: url, options: .atomic)
    }
}
```

- [ ] **Step 2: Write the tests**

Create `auto2fa-mac/Auto2FATests/ManagedHostStoreTests.swift`:

```swift
import XCTest

final class ManagedHostStoreTests: XCTestCase {
    private func tmp() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("mhs-\(UUID().uuidString)")
            .appendingPathComponent("managed_hosts.json")
    }

    func testMissingFileLoadsEmpty() {
        XCTAssertTrue(ManagedHostStore.load(from: tmp()).isEmpty)
    }

    func testUpsertRoundTrips() throws {
        let url = tmp()
        let c = ManagedHostConn(alias: "cannon", hostName: "login.rc.fas.harvard.edu",
                                user: "jdoe", port: 22)
        try ManagedHostStore.upsert(c, in: url)
        let back = ManagedHostStore.load(from: url)
        XCTAssertEqual(back, [c])
    }

    func testUpsertReplacesByAlias() throws {
        let url = tmp()
        try ManagedHostStore.upsert(ManagedHostConn(alias: "a", hostName: "h1", user: "u", port: 22), in: url)
        try ManagedHostStore.upsert(ManagedHostConn(alias: "a", hostName: "h2", user: "u", port: 22), in: url)
        let back = ManagedHostStore.load(from: url)
        XCTAssertEqual(back.count, 1)
        XCTAssertEqual(back.first?.hostName, "h2")
    }

    func testRemove() throws {
        let url = tmp()
        try ManagedHostStore.upsert(ManagedHostConn(alias: "a", hostName: "h", user: "u", port: 22), in: url)
        try ManagedHostStore.remove(alias: "a", in: url)
        XCTAssertTrue(ManagedHostStore.load(from: url).isEmpty)
    }
}
```

- [ ] **Step 3: Add both files to the test target**

In `auto2fa-mac/project.yml`, under the `Auto2FATests:` target `sources:` list (where `Auto2FA/FriendlyText.swift` etc. are listed), add:

```yaml
      - path: Auto2FA/ManagedHostStore.swift
```

(The test file under `Auto2FATests/` is already covered by `- path: Auto2FATests`.)

- [ ] **Step 4: Regenerate + run the tests**

Run:
```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodegen generate >/dev/null && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -configuration Debug -destination 'platform=macOS' test 2>&1 | grep -E "ManagedHostStoreTests|TEST (SUCCEEDED|FAILED)" | tail -8
```
Expected: the 4 ManagedHostStore tests pass; `** TEST SUCCEEDED **`.

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-mac/Auto2FA/ManagedHostStore.swift auto2fa-mac/Auto2FATests/ManagedHostStoreTests.swift auto2fa-mac/project.yml
git commit -m "feat(ui): sidecar store for guided-host connection fields (alias -> host/user/port)"
```

---

## Task 7: Config generator — connection blocks + daemon wrapper + sanitize/conflict

**Files:**
- Modify: `auto2fa-mac/Auto2FA/SSHConfigManager.swift`
- Create: `auto2fa-mac/Auto2FATests/SSHConfigGenTests.swift`
- Modify: `auto2fa-mac/project.yml` (SSHConfigManager + SSHPaths into the test target if not already)

- [ ] **Step 1: Write the failing tests**

Create `auto2fa-mac/Auto2FATests/SSHConfigGenTests.swift`:

```swift
import XCTest

final class SSHConfigGenTests: XCTestCase {
    private let dir = "/Users/x/.ssh"

    func testManagedBlockWithConnEmitsHostNameUserPort() {
        let conf = SSHConfigManager.generateManagedConf(
            hosts: [.init(alias: "cannon", conn: .init(hostName: "login.example.edu", user: "jdoe", port: 2222))],
            dir: dir)
        XCTAssertTrue(conf.contains("Host cannon"))
        XCTAssertTrue(conf.contains("HostName login.example.edu"))
        XCTAssertTrue(conf.contains("User jdoe"))
        XCTAssertTrue(conf.contains("Port 2222"))
        XCTAssertTrue(conf.contains("ControlMaster auto"))
        // The managed file itself must NOT include anything (avoids circular
        // include when ~/.ssh/config includes it for terminal reuse).
        XCTAssertFalse(conf.contains("Include"))
    }

    func testManagedBlockWithoutConnIsControlMasterOnly() {
        let conf = SSHConfigManager.generateManagedConf(
            hosts: [.init(alias: "legacy", conn: nil)], dir: dir)
        XCTAssertTrue(conf.contains("Host legacy"))
        XCTAssertTrue(conf.contains("ControlMaster auto"))
        XCTAssertFalse(conf.contains("HostName"))
        XCTAssertFalse(conf.contains("User "))
    }

    func testPort22IsOmitted() {
        // Default port is implicit — don't clutter the file with Port 22.
        let conf = SSHConfigManager.generateManagedConf(
            hosts: [.init(alias: "h", conn: .init(hostName: "a", user: "u", port: 22))], dir: dir)
        XCTAssertFalse(conf.contains("Port 22"))
    }

    func testDaemonWrapperIncludesManagedThenUserConfig() {
        let w = SSHConfigManager.daemonWrapperContent(dir: dir)
        let mIdx = w.range(of: "Include \(dir)/ssh2fa.conf")
        let uIdx = w.range(of: "Include \(dir)/config")
        XCTAssertNotNil(mIdx); XCTAssertNotNil(uIdx)
        XCTAssertTrue(mIdx!.lowerBound < uIdx!.lowerBound, "managed hosts must come before user config (first-value-wins)")
    }

    func testSanitizeAlias() {
        XCTAssertEqual(SSHConfigManager.sanitizeAlias("My Lab Server!"), "My-Lab-Server")
        XCTAssertEqual(SSHConfigManager.sanitizeAlias("login.rc.fas.harvard.edu"), "login.rc.fas.harvard.edu")
        XCTAssertEqual(SSHConfigManager.sanitizeAlias("  a  b  "), "a-b")
    }

    func testConflictDetection() {
        // Conflicts only against the user's OWN config aliases (passed in).
        XCTAssertTrue(SSHConfigManager.aliasConflicts("cannon", userAliases: ["cannon", "other"]))
        XCTAssertFalse(SSHConfigManager.aliasConflicts("fresh", userAliases: ["cannon"]))
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run (after adding the file to the test target in Step 4): it won't compile yet — `generateManagedConf(hosts:)`, `daemonWrapperContent`, `sanitizeAlias`, `aliasConflicts` don't exist. (You can run after Step 3+4; expected FAIL → then PASS.)

- [ ] **Step 3: Implement in SSHConfigManager**

In `SSHConfigManager.swift`, add a connection type + change the generator. Replace the existing `generateManagedConf(aliases:dir:)` with a `hosts:`-based one (and add the helpers):

```swift
    /// A host to render into the managed conf. `conn == nil` → a legacy/imported
    /// alias that relies on the user's own config; emit only the ControlMaster
    /// block (today's behavior). `conn != nil` → a guided host; emit the full
    /// connection definition so it resolves with no user-config entry.
    struct ManagedHost {
        var alias: String
        var conn: Conn?
        struct Conn { var hostName: String; var user: String; var port: Int }
    }

    static func generateManagedConf(hosts: [ManagedHost], dir: String) -> String {
        let header = "# Managed by SSH2FA — do not edit. Regenerated on host add/remove.\n"
        let blocks = hosts.sorted { $0.alias < $1.alias }.map { h -> String in
            let cp = SSHPaths.controlPathFallback(dir: dir, alias: h.alias)
            var lines = ["Host \(h.alias)"]
            if let c = h.conn {
                lines.append("    HostName \(c.hostName)")
                lines.append("    User \(c.user)")
                if c.port != 22 { lines.append("    Port \(c.port)") }
            }
            lines.append("    ControlMaster auto")
            lines.append("    ControlPath \(cp)")
            lines.append("    ControlPersist yes")
            return lines.joined(separator: "\n")
        }
        return header + "\n" + blocks.joined(separator: "\n\n") + (blocks.isEmpty ? "" : "\n")
    }

    /// The daemon wrapper (~/.ssh/ssh2fa-daemon.conf) that `ssh -F` reads:
    /// our managed hosts FIRST (so their values win), then the user's config to
    /// inherit globals + legacy hosts. The managed file has no includes, so this
    /// one-directional include chain can never loop.
    static func daemonWrapperContent(dir: String) -> String {
        """
        # Managed by SSH2FA — the daemon reads this via `ssh -F`. Do not edit.
        Include \(dir)/ssh2fa.conf
        Include \(dir)/config
        """ + "\n"
    }

    /// Reduce a user-facing name to a legal ssh `Host` token: trim, collapse
    /// whitespace runs to a single `-`, drop characters ssh treats specially.
    static func sanitizeAlias(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let collapsed = trimmed.replacingOccurrences(of: "\\s+", with: "-",
                                                     options: .regularExpression)
        let allowed = collapsed.unicodeScalars.filter {
            CharacterSet.alphanumerics.contains($0) || "-._".unicodeScalars.contains($0)
        }
        return String(String.UnicodeScalarView(allowed))
    }

    /// True iff `alias` is already a Host the USER defined in their own config.
    static func aliasConflicts(_ alias: String, userAliases: [String]) -> Bool {
        userAliases.contains(alias)
    }
```

IMPORTANT — keep the tree compiling: **ADD** the new `generateManagedConf(hosts:dir:)` as an OVERLOAD and leave the existing `generateManagedConf(aliases:dir:)` UNTOUCHED for now (Swift distinguishes them by the `hosts:`/`aliases:` label). Do NOT touch `writeManagedConf` or any callers in this task — the old `aliases:` generator + its callers keep compiling. Task 8 switches `writeManagedConf` + the (single) caller to the `hosts:` form and then deletes the old `aliases:` generator.

Verify `SSHPaths.controlPathFallback(dir:alias:)` exists (it's used in the current generator at line 28) — reuse it.

- [ ] **Step 4: Add the test file deps to the test target**

In `project.yml`, ensure the test target sources include `Auto2FA/SSHConfigManager.swift` and `Auto2FA/SSHPaths.swift` (SSHPaths is already there per the existing list; add SSHConfigManager if not already present):

```yaml
      - path: Auto2FA/SSHConfigManager.swift
```

- [ ] **Step 5: Regenerate + run**

Run:
```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodegen generate >/dev/null && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -configuration Debug -destination 'platform=macOS' test 2>&1 | grep -E "SSHConfigGenTests|TEST (SUCCEEDED|FAILED)" | tail -10
```
Expected: the SSHConfigGen tests pass; `** TEST SUCCEEDED **`.

- [ ] **Step 6: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-mac/Auto2FA/SSHConfigManager.swift auto2fa-mac/Auto2FATests/SSHConfigGenTests.swift auto2fa-mac/project.yml
git commit -m "feat(ui): managed conf emits connection blocks + daemon wrapper + alias sanitize/conflict"
```

---

## Task 8: AppState wiring — write config on add + always-regenerate

**Files:**
- Modify: `auto2fa-mac/Auto2FA/AppState.swift`
- Modify: `auto2fa-mac/Auto2FA/SSHConfigManager.swift` (`writeManagedConf` + a `writeDaemonWrapper`)

- [ ] **Step 1: Add `writeManagedConf(hosts:)` + `writeDaemonWrapper` to SSHConfigManager**

In `SSHConfigManager.swift`, update `writeManagedConf` to take `[ManagedHost]` and add a wrapper writer (both atomic, skip-if-unchanged like the existing one):

```swift
    /// Write ~/.ssh/ssh2fa.conf from the host list. Returns true if it changed.
    @discardableResult
    static func writeManagedConf(hosts: [ManagedHost], dir: String) throws -> Bool {
        let path = (dir as NSString).appendingPathComponent("ssh2fa.conf")
        let content = generateManagedConf(hosts: hosts, dir: dir)
        return try writeIfChanged(content, to: path, perms: 0o600)
    }

    /// Write ~/.ssh/ssh2fa-daemon.conf (the `-F` wrapper). Returns true if changed.
    @discardableResult
    static func writeDaemonWrapper(dir: String) throws -> Bool {
        let path = (dir as NSString).appendingPathComponent("ssh2fa-daemon.conf")
        return try writeIfChanged(daemonWrapperContent(dir: dir), to: path, perms: 0o600)
    }
```

If a private `writeIfChanged`/`atomicWrite` helper already exists (the current `writeManagedConf(aliases:)` uses one — it "skips unchanged content"), reuse it; otherwise factor the existing skip-if-unchanged + atomicWrite logic into `writeIfChanged(_:to:perms:)`.

After `writeManagedConf(hosts:)` compiles and its single old caller (the `WarmReuseConsent` + any `AppState` call to `writeManagedConf(aliases:)`) is switched to the `hosts:` form in Step 2 below, **delete the now-unused old `generateManagedConf(aliases:dir:)` and `writeManagedConf(aliases:dir:)`** (added-as-overloads in Task 7). Grep `generateManagedConf(aliases:` / `writeManagedConf(aliases:` to confirm zero remaining callers before deleting.

- [ ] **Step 2: Replace `syncSSHConfigIfEnabled` with an always-on managed-config sync**

In `AppState.swift`, the current `syncSSHConfigIfEnabled()` (around line 821) is gated on `warmReuseEnabled` and writes from `aliases`. Replace it so it ALWAYS writes the managed conf + wrapper (the daemon needs them regardless of the terminal-reuse opt-in), pulling connection fields from the sidecar:

```swift
    /// Regenerate ssh2fa.conf + the daemon wrapper from the live host list and
    /// the sidecar. ALWAYS runs (the daemon resolves via these files) — the
    /// terminal-reuse Include into ~/.ssh/config stays a separate opt-in.
    /// Safe on every reload (writes skip unchanged content).
    func syncManagedSSHConfig() {
        let dir = SSHPaths.sshDir()
        let sidecar = ManagedHostStore.load(from: managedHostsURL)
        let byAlias = Dictionary(uniqueKeysWithValues: sidecar.map { ($0.alias, $0) })
        let managed: [SSHConfigManager.ManagedHost] = hosts.map { h in
            if let c = byAlias[h.host] {
                return .init(alias: h.host, conn: .init(hostName: c.hostName, user: c.user, port: c.port))
            }
            return .init(alias: h.host, conn: nil)
        }
        try? SSHConfigManager.writeManagedConf(hosts: managed, dir: dir)
        try? SSHConfigManager.writeDaemonWrapper(dir: dir)
    }

    /// Sidecar location: ~/.ssh2fa/managed_hosts.json.
    var managedHostsURL: URL {
        URL(fileURLWithPath: NSHomeDirectory())
            .appendingPathComponent(".ssh2fa")
            .appendingPathComponent("managed_hosts.json")
    }
```

Then update every CALLER of the old `syncSSHConfigIfEnabled()` to call `syncManagedSSHConfig()` (grep for it). Also update the old `WarmReuseConsent.writeManagedConf(aliases:)` call site to the new `hosts:` form (Task 7 changed the signature) — for the consent path, pass the same `hosts` mapping. (If that's only reachable when warm-reuse is enabled, compute the mapping there too.)

- [ ] **Step 3: Add `addManagedHost` to AppState**

In `AppState.swift`, add a method the wizard calls to register a guided host (write sidecar + regen config + then the existing addHost):

```swift
    /// Guided add: persist the connection fields, regenerate the managed ssh
    /// config + wrapper, THEN register credentials (so the test-login + master
    /// resolve the new host via `ssh -F`). Returns nil on success or an error.
    func addManagedHost(alias: String, hostName: String, user: String, port: Int,
                        password: String, otpauthURL: String, autoConnect: Bool) async -> String? {
        do {
            try ManagedHostStore.upsert(
                ManagedHostConn(alias: alias, hostName: hostName, user: user, port: port),
                in: managedHostsURL)
        } catch {
            return "Couldn't save connection settings: \(error.localizedDescription)"
        }
        syncManagedSSHConfig()           // writes ssh2fa.conf + wrapper before login
        return await addHost(host: alias, password: password,
                             otpauthURL: otpauthURL, autoConnect: autoConnect)
    }
```

- [ ] **Step 4: Build the app**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -configuration Debug -destination 'platform=macOS' build 2>&1 | grep -E "BUILD SUCCEEDED|BUILD FAILED|error:" | head`
Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 5: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-mac/Auto2FA/AppState.swift auto2fa-mac/Auto2FA/SSHConfigManager.swift
git commit -m "feat(ui): always-on managed ssh config sync + addManagedHost (sidecar -> conf -> wrapper -> register)"
```

---

## Task 9: AddHostSheet redesign — collect connection fields

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/AddHostSheet.swift`

**MODE BRANCHING (critical):** `AddHostSheet` is constructed two ways — `AddHostSheet()` (manual "Add a host" → the new GUIDED flow) and `AddHostSheet(prefillAlias: <configAlias>)` (the import-from-`~/.ssh/config` flow, which registers an alias the user ALREADY defined). Grep `AddHostSheet(prefillAlias:` to confirm the import caller(s). The import path must KEEP today's behavior: a single alias field, NO address/username collection, and `addHost(...)` (conn = nil — it resolves via the user's own config, picked up by the wrapper's `Include ~/.ssh/config`). Define `private var isGuided: Bool { prefillAlias == nil }` and branch the field UI + `advance()`/`testLogin()`/`submit()` on it: when `isGuided` use the new fields/logic below; otherwise leave the existing single-field/`advance`/`testLogin`/`submit` code paths exactly as they are today. The steps below describe the `isGuided` branch.

- [ ] **Step 1: Add the connection fields + state**

In `AddHostSheet.swift`, add state for the new fields (near the existing `@State private var hostname`):

```swift
    @State private var displayName = ""        // friendly → sanitized alias
    @State private var serverAddress = ""      // HostName
    @State private var username = ""           // User
    @State private var portText = "22"         // Port (advanced)
    @State private var showAdvanced = false
```

- [ ] **Step 2: Add the guided fields to `stepConnection` (the `isGuided` branch)**

In `stepConnection`, wrap the field area in `if isGuided { …new guided fields… } else { …the existing `field("Hostname or SSH alias", …)` block + its `hostInConfig` warning, unchanged… }`. The guided branch's fields (Name drives the alias):

```swift
                field("Name", TextField("e.g. Cannon, lab server", text: $displayName)
                        .focused($focused, equals: .hostname))
                field("Server address", TextField("login.rc.fas.harvard.edu", text: $serverAddress))
                field("Username", TextField("your login name on the server", text: $username))
                DisclosureGroup("Advanced", isExpanded: $showAdvanced) {
                    field("Port", TextField("22", text: $portText))
                }
```

(Keep the existing Password + 2FA fields below, unchanged.)

- [ ] **Step 3: Update `advance()` validation (isGuided branch)**

In `advance()`, branch on `isGuided`: keep the existing guards for the import path; for the guided path use these guards (validate name/address/username + conflict):

```swift
    private func advance() {
        let name = SSHConfigManager.sanitizeAlias(displayName)
        guard !name.isEmpty else { error = "Give this host a name."; focused = .hostname; return }
        guard !serverAddress.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            error = "Enter the server address."; return
        }
        guard !username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            error = "Enter your username on the server."; return
        }
        if SSHConfigManager.aliasConflicts(name, userAliases: appState.parsedConfig.hosts.map { $0.alias }) {
            error = "You already have an SSH host named “\(name)”. Pick a different name."
            focused = .hostname; return
        }
        guard !password.isEmpty else { error = "Password is required."; return }
        guard OTPSecret.normalize(input: otpauthURL, account: name) != nil else {
            error = "Enter a 2FA secret — an otpauth:// URL or a base32 key."
            focused = .otpauth; return
        }
        error = nil
        step = 1
    }
```

- [ ] **Step 4: Update `testLogin()` to write config first, test the alias**

`testLogin` must ensure the managed config exists (so `ssh -F` resolves the new alias) before testing. Change it to write the sidecar+conf for the in-progress host first, then test against the sanitized alias:

```swift
    private func testLogin() async {
        guard !testing else { return }
        testing = true; testResult = nil; error = nil
        let alias = SSHConfigManager.sanitizeAlias(displayName)
        let port = Int(portText.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 22
        // Write the connection into the managed config so `ssh -F` resolves it.
        try? ManagedHostStore.upsert(
            ManagedHostConn(alias: alias,
                            hostName: serverAddress.trimmingCharacters(in: .whitespacesAndNewlines),
                            user: username.trimmingCharacters(in: .whitespacesAndNewlines), port: port),
            in: appState.managedHostsURL)
        appState.syncManagedSSHConfig()
        do {
            let (ok, reason) = try await appState.client.testHostCredentials(
                host: alias, password: password,
                otpauthURL: OTPSecret.normalize(input: otpauthURL, account: alias)
                    ?? otpauthURL.trimmingCharacters(in: .whitespacesAndNewlines))
            testResult = (ok, ok ? "Login succeeded — you can save now." : reason)
        } catch {
            testResult = (false, "Test couldn't run: \(error.localizedDescription)")
        }
        testing = false
    }
```

- [ ] **Step 5: Update `submit()` to call `addManagedHost`**

```swift
    private func submit() {
        guard !submitting else { return }
        submitting = true; error = nil
        let alias = SSHConfigManager.sanitizeAlias(displayName)
        let port = Int(portText.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 22
        Task {
            if let msg = await appState.addManagedHost(
                alias: alias,
                hostName: serverAddress.trimmingCharacters(in: .whitespacesAndNewlines),
                user: username.trimmingCharacters(in: .whitespacesAndNewlines),
                port: port,
                password: password,
                otpauthURL: OTPSecret.normalize(input: otpauthURL, account: alias)
                    ?? otpauthURL.trimmingCharacters(in: .whitespacesAndNewlines),
                autoConnect: autoConnect
            ) {
                error = msg; submitting = false
            } else {
                appState.dismissSheet()
            }
        }
    }
```

(Remove the now-unused `prefillAlias`-returns-to-import branch only if `prefillAlias` is no longer used. If the import path still constructs `AddHostSheet(prefillAlias:)`, keep `prefillAlias` populating `displayName`/`serverAddress` as before — the import path is out of Phase-1 scope; just make sure it still compiles. Verify by grepping callers of `AddHostSheet(prefillAlias:`.)

- [ ] **Step 6: Update the confirm-step summary + remove the stale `hostname`/`aliasKnown` warning UI**

Replace any `Text(hostname)` in `stepConfirm` with `Text(SSHConfigManager.sanitizeAlias(displayName))` and drop the `hostInConfig`/`aliasKnown` warning (it doesn't apply to the guided flow). Keep the OTP-ready check (use `alias` as the account).

- [ ] **Step 7: Build + manual QA**

Run: `cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -configuration Debug -destination 'platform=macOS' build 2>&1 | grep -E "BUILD SUCCEEDED|BUILD FAILED|error:" | head`
Expected: `** BUILD SUCCEEDED **`.

Manual QA (with an empty or simple `~/.ssh/config`): Add Host → fill Name/Address/Username + password + 2FA → Test login resolves the new alias → Save → host comes up; `cat ~/.ssh/ssh2fa.conf` shows the `Host` block with HostName/User; `cat ~/.ssh/ssh2fa-daemon.conf` shows the two Includes; the user's `~/.ssh/config` is unchanged.

- [ ] **Step 8: Commit**

```bash
cd /Users/shgao/logs/auto2fa_dev
git add auto2fa-mac/Auto2FA/Views/AddHostSheet.swift
git commit -m "feat(ui): guided Add Host collects name/address/username + writes ssh config before login"
```

---

## Final verification

- [ ] **Rust suite + Swift tests + app build all green**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-rs && cargo test --workspace -- --test-threads=1 2>&1 | grep -cE "test result: FAILED" | xargs echo "Rust FAILED suites:"
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -configuration Debug -destination 'platform=macOS' test 2>&1 | grep -E "TEST (SUCCEEDED|FAILED)" | tail -1
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -configuration Debug -destination 'platform=macOS' build 2>&1 | grep -E "BUILD SUCCEEDED|BUILD FAILED" | tail -1
```
Expected: `Rust FAILED suites: 0`, `** TEST SUCCEEDED **`, `** BUILD SUCCEEDED **`.

Then proceed to `superpowers:finishing-a-development-branch`. **Phase 2** (self-heal reconcile on launch/reload + auto-enable the terminal Include with revert + the "missing address" non-blocking prompt) is a separate plan written after Phase 1 is validated.
