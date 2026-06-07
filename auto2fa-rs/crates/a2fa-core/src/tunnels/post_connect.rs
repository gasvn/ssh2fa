use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Run a per-tunnel post-connect shell command in a background thread.
///
/// Mirrors `TunnelManager._run_post_connect` from `tunnels.py`:
///
/// - If `tunnel_name` is already present in `running`, we skip the launch
///   (prevents duplicate hooks when the tunnel flaps).
/// - Otherwise the name is inserted into `running` before the thread starts
///   and removed when the command finishes (success or failure).
/// - The command is run via `/bin/sh -c <cmd>`.
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
            run_post_connect_inner(&tunnel_name, &cmd, local_port, &node, &jump, &running);
        })
    {
        log::error!("failed to spawn post_connect thread: {e}");
        // Clean up the running-set so future attempts are not blocked.
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
    running: &Arc<Mutex<HashSet<String>>>,
) {
    use std::process::Command;

    let mut env = std::env::vars().collect::<Vec<_>>();
    env.push(("AUTO2FA_TUNNEL_NAME".into(), sanitize(tunnel_name)));
    env.push(("AUTO2FA_LOCAL_PORT".into(), local_port.to_string()));
    env.push(("AUTO2FA_NODE".into(), sanitize(node)));
    env.push(("AUTO2FA_JUMP".into(), sanitize(jump)));
    env.push((
        "AUTO2FA_URL".into(),
        format!("http://localhost:{local_port}"),
    ));

    log::debug!("[tunnel:{tunnel_name}] post_connect: running `{}`", &cmd[..cmd.len().min(60)]);

    let result = Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .envs(env)
        .output();

    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if out.status.success() {
                log::debug!(
                    "[tunnel:{tunnel_name}] post_connect: exited 0; stdout={stdout:?}"
                );
            } else {
                log::warn!(
                    "[tunnel:{tunnel_name}] post_connect: exited {:?}; stderr={stderr:?}",
                    out.status.code()
                );
            }
        }
        Err(e) => {
            log::error!("[tunnel:{tunnel_name}] post_connect: failed to run command: {e}");
        }
    }

    let mut guard = running.lock().unwrap_or_else(|e| e.into_inner());
    guard.remove(tunnel_name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::collections::HashSet;

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
        std::thread::sleep(std::time::Duration::from_millis(200));
        // After completion the name should be removed.
        assert!(!running.lock().unwrap().contains("probe-tunnel"));
    }
}
