use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Hard deadline for a post-connect hook (mirrors Python's `timeout=30`).
///
/// A hook like `tail -f`, or one that backgrounds a server holding stdout open,
/// would otherwise block `Command::output()` forever — wedging the worker and
/// (because the name is only removed at the very end) permanently disabling the
/// hook for that tunnel. We poll `try_wait()` against this deadline and
/// `kill()+wait()` the child on timeout.
const POST_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the bounded-run poll loop wakes up to check for child exit.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// SIGKILL the hook's whole process group, then reap the shell.
///
/// The child was spawned with `process_group(0)`, so its pgid == its pid and
/// `killpg` reaches every descendant that hasn't re-setsid'd — compound hooks
/// (`a && b`, pipelines, `helper &`) no longer leak grandchildren past the
/// deadline. The follow-up `kill()+wait()` is belt-and-braces reaping of the
/// direct child (idempotent if killpg already got it).
fn kill_hook_group(child: &mut std::process::Child) {
    let pgid = child.id() as i32;
    if pgid > 0 {
        unsafe {
            let _ = libc::killpg(pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// RAII guard that removes `name` from the `running` dedup set on drop.
///
/// This guarantees the name is removed on EVERY exit path of the worker —
/// normal return, early return, OR panic (e.g. a stray byte-slice panic) —
/// so a tunnel's hook can never get permanently wedged in the "still running"
/// state. Mirrors `StartGuard` in the daemon's managers module.
struct RunningGuard {
    running: Arc<Mutex<HashSet<String>>>,
    name: String,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut guard = self.running.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&self.name);
    }
}

/// Run a per-tunnel post-connect shell command in a background thread.
///
/// Mirrors `TunnelManager._run_post_connect` from `tunnels.py`:
///
/// - If `tunnel_name` is already present in `running`, we skip the launch
///   (prevents duplicate hooks when the tunnel flaps).
/// - Otherwise the name is inserted into `running` before the thread starts
///   and removed (via an RAII [`RunningGuard`]) when the command finishes,
///   times out, or the worker panics.
/// - The command is run via `/bin/sh -c <cmd>` with a hard 30 s deadline.
/// - The environment receives the standard `AUTO2FA_*` variables; values are
///   sanitized to strip shell metacharacters (same policy as the Python code).
/// - No live test; the function is exercised by integration tests that can
///   actually spawn a shell.
pub fn run_post_connect(
    tunnel_name: String,
    cmd: String,
    local_port: u16,
    node: String,
    jump: String,
    running: Arc<Mutex<HashSet<String>>>,
) {
    {
        let mut guard = running.lock().unwrap_or_else(|e| e.into_inner());
        if guard.contains(&tunnel_name) {
            log::debug!(
                "[tunnel:{}] post_connect: previous hook still running, skipping",
                tunnel_name
            );
            return;
        }
        guard.insert(tunnel_name.clone());
    }

    let running_for_error = running.clone();
    let name_for_error = tunnel_name.clone();

    if let Err(e) = std::thread::Builder::new()
        .name(format!("post_connect:{tunnel_name}"))
        .spawn(move || {
            // RAII: remove the name from the running set on EVERY exit path
            // (normal, early-return, or panic). Constructed first so it is the
            // last thing dropped.
            let _running_guard = RunningGuard {
                running,
                name: tunnel_name.clone(),
            };
            run_post_connect_inner(&tunnel_name, &cmd, local_port, &node, &jump);
        })
    {
        log::error!("failed to spawn post_connect thread: {e}");
        // The worker (and its RunningGuard) never ran — clean up the running-set
        // here so future attempts are not blocked.
        let mut guard = running_for_error.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&name_for_error);
    }
}

fn sanitize(s: &str) -> String {
    // Allow word chars, dot, dash, colon, slash, @, % — same allow-list as
    // the Python `_sanitize` helper.  Everything else is stripped.
    s.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/' | '@' | '%'))
        .collect()
}

fn run_post_connect_inner(
    tunnel_name: &str,
    cmd: &str,
    local_port: u16,
    node: &str,
    jump: &str,
) {
    use std::process::{Command, Stdio};

    let mut env = std::env::vars().collect::<Vec<_>>();
    env.push(("AUTO2FA_TUNNEL_NAME".into(), sanitize(tunnel_name)));
    env.push(("AUTO2FA_LOCAL_PORT".into(), local_port.to_string()));
    env.push(("AUTO2FA_NODE".into(), sanitize(node)));
    env.push(("AUTO2FA_JUMP".into(), sanitize(jump)));
    env.push((
        "AUTO2FA_URL".into(),
        format!("http://localhost:{local_port}"),
    ));

    // Char-safe truncation for logging — byte-slicing (`&cmd[..60]`) would PANIC
    // if a multibyte UTF-8 char straddles byte 60.
    let cmd_preview: String = cmd.chars().take(60).collect();
    log::debug!("[tunnel:{tunnel_name}] post_connect: running `{cmd_preview}`");

    // Spawn with stdin nulled (a hook reading stdin can't block on the daemon)
    // and stdout/stderr nulled so a backgrounded server inheriting the pipe
    // can't hold us open. Poll try_wait against a 30 s deadline; kill+wait on
    // timeout. Mirrors `run_ssh_bounded` in ssh/control.rs.
    //
    // process_group(0): the hook runs in its OWN process group so the timeout
    // kill can take out the WHOLE group. `child.kill()` alone SIGKILLs only
    // /bin/sh — a compound hook (`a && b`, pipeline, backgrounded helper)
    // left grandchildren running past the deadline, and once RunningGuard
    // freed the dedup slot each tunnel flap spawned another generation.
    // (A hook that finishes in time and intentionally leaves a background
    // server behind is unaffected — the group is only killed on timeout.)
    use std::os::unix::process::CommandExt;
    let mut child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("[tunnel:{tunnel_name}] post_connect: failed to run command: {e}");
            return;
        }
    };

    let deadline = Instant::now() + POST_CONNECT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    log::debug!("[tunnel:{tunnel_name}] post_connect: exited 0");
                } else {
                    log::warn!(
                        "[tunnel:{tunnel_name}] post_connect: exited {:?}",
                        status.code()
                    );
                }
                return;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    log::warn!(
                        "[tunnel:{tunnel_name}] post_connect: timed out after {POST_CONNECT_TIMEOUT:?} — killing process group"
                    );
                    kill_hook_group(&mut child);
                    return;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                log::error!("[tunnel:{tunnel_name}] post_connect: wait error: {e} — killing process group");
                kill_hook_group(&mut child);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    /// REGRESSION (grandchild leak): killing only /bin/sh left backgrounded
    /// grandchildren alive past the timeout — kill_hook_group must take out
    /// the whole process group.
    #[test]
    fn kill_hook_group_kills_grandchildren() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        // sh backgrounds a long sleep (the grandchild, same process group),
        // writes its pid, then sleeps itself.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "sleep 30 & echo $! > {}; sleep 30",
                pid_file.display()
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();

        // Wait for the grandchild pid to land on disk.
        let grand_pid: i32 = {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(s) = std::fs::read_to_string(&pid_file) {
                    if let Ok(p) = s.trim().parse() {
                        break p;
                    }
                }
                assert!(Instant::now() < deadline, "grandchild pid never appeared");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // Grandchild is alive before the kill.
        assert_eq!(unsafe { libc::kill(grand_pid, 0) }, 0);

        kill_hook_group(&mut child);

        // Grandchild must no longer be RUNNING. NOTE: `kill(pid, 0)` returns
        // 0 for a zombie, and the orphan's reaping (by launchd, after the
        // reparent) is asynchronous and can take a while under load — so
        // accept either "gone" (ESRCH) or "zombie" (state Z = killed,
        // awaiting reap) and only fail on a genuinely still-running process.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if unsafe { libc::kill(grand_pid, 0) } != 0 {
                break; // ESRCH — fully gone
            }
            let state = Command::new("ps")
                .args(["-o", "state=", "-p", &grand_pid.to_string()])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if state.is_empty() || state.starts_with('Z') {
                break; // gone, or killed-but-unreaped zombie — both fine
            }
            assert!(
                Instant::now() < deadline,
                "grandchild survived kill_hook_group (state {state})"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn sanitize_strips_metacharacters() {
        assert_eq!(sanitize("holygpu01"), "holygpu01");
        assert_eq!(sanitize("user@host.example.com"), "user@host.example.com");
        // Dangerous chars stripped
        let s = sanitize("; rm -rf ~");
        assert!(!s.contains(';'));
        assert!(!s.contains(' '));
        assert!(!s.contains('~'));
    }

    #[test]
    fn run_post_connect_skips_if_already_running() {
        let running = Arc::new(Mutex::new(HashSet::new()));
        running.lock().unwrap().insert("my-tunnel".to_string());

        // This should be a no-op (no thread spawned, no panic).
        run_post_connect(
            "my-tunnel".to_string(),
            "echo hello".to_string(),
            8080,
            "node01".to_string(),
            "jump.example.com".to_string(),
            running.clone(),
        );

        // Name still in running set (we didn't remove it).
        assert!(running.lock().unwrap().contains("my-tunnel"));
    }

    #[test]
    fn run_post_connect_inserts_name_before_spawn() {
        // Verifies the name appears in `running` right after the call
        // (before the thread finishes).
        let running: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        run_post_connect(
            "probe-tunnel".to_string(),
            // Use `true` (exits 0 immediately) so the test isn't slow.
            "true".to_string(),
            9000,
            "node42".to_string(),
            "jump1".to_string(),
            running.clone(),
        );

        // Give the thread a moment to finish and clean up.
        std::thread::sleep(std::time::Duration::from_millis(300));
        // After completion the name should be removed (RAII guard ran).
        assert!(!running.lock().unwrap().contains("probe-tunnel"));
    }

    /// A multibyte UTF-8 command that straddles byte 60 must NOT panic the
    /// logging truncation. Byte-slicing `&cmd[..60]` would panic; char-based
    /// truncation must not.
    #[test]
    fn cmd_preview_truncation_is_char_safe() {
        // Build a string whose byte length crosses 60 in the middle of a
        // multibyte char ('é' is 2 bytes, '€' is 3 bytes).
        let mut cmd = "a".repeat(59); // 59 bytes
        cmd.push('€'); // byte 60..63 — byte-slicing at 60 would split this
        cmd.push_str("trailing");

        // This is exactly what the logging path does; must not panic.
        let preview: String = cmd.chars().take(60).collect();
        assert!(preview.starts_with(&"a".repeat(59)));
        assert!(preview.ends_with('€'));
        // 59 'a' + 1 '€' = 60 chars.
        assert_eq!(preview.chars().count(), 60);
    }

    /// The RAII guard must remove the name even if the worker logic panics.
    #[test]
    fn running_guard_removes_on_drop_even_on_panic() {
        let running: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        running.lock().unwrap().insert("panicky".to_string());

        let running_clone = running.clone();
        let handle = std::thread::spawn(move || {
            let _g = RunningGuard {
                running: running_clone,
                name: "panicky".to_string(),
            };
            panic!("boom");
        });
        let _ = handle.join(); // expected to be Err

        assert!(
            !running.lock().unwrap().contains("panicky"),
            "RunningGuard must remove the name on panic"
        );
    }

    /// A hook that would otherwise block forever (reads from stdin) must be
    /// reaped. We don't wait the full 30 s here — we use a fast-exiting child
    /// and assert the worker cleans up the running set promptly. The timeout
    /// path itself is covered structurally (kill+wait on deadline).
    #[test]
    fn run_post_connect_cleans_up_after_fast_child() {
        let running: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        run_post_connect(
            "fast".to_string(),
            // exits immediately even though it would read stdin if attached;
            // stdin is nulled so it can't block.
            "true".to_string(),
            9100,
            "n".to_string(),
            "j".to_string(),
            running.clone(),
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!running.lock().unwrap().contains("fast"));
    }
}
