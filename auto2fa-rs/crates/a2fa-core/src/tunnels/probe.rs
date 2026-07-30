use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// Returns `true` if port is free to bind on 127.0.0.1.
///
/// Attempts to bind a TCP listener; succeeds → port is available.
/// Any failure (EADDRINUSE, permission denied, etc.) → not available.
pub fn port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

const PROBE_INTERVAL: Duration = Duration::from_millis(200);

/// Poll `127.0.0.1:port` with repeated TCP connect attempts until the port
/// accepts a connection or `timeout` elapses.
///
/// Returns `true` if a connection succeeds before the deadline, `false`
/// otherwise.  Each individual connect attempt has a 500 ms timeout so the
/// total wall time is at most `timeout + 500 ms`.
pub fn probe_port_ready(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(500),
        )
        .is_ok()
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(PROBE_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_port_is_unavailable() {
        // Busy direction is deterministic: WE hold the listener.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(!port_available(port), "a port we hold must not be available");
        drop(l);

        // Free direction is inherently racy: the OS can hand a just-released
        // ephemeral port to ANOTHER process before we re-check, and then
        // `port_available` correctly returning false is not a bug. (This flaked
        // in a real run.) Give it several independent ports — a genuine
        // regression fails every attempt, a stolen port fails at most a few.
        let saw_free = (0..5).any(|_| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let p = l.local_addr().unwrap().port();
            drop(l);
            port_available(p)
        });
        assert!(saw_free, "a released port must be reported available");
    }

    #[test]
    fn probe_times_out_on_closed_port() {
        // Same race as above, inverted: if another process grabs the released
        // port, the probe legitimately connects. Retry across fresh ports — a
        // broken probe (one that never times out) fails all five.
        let timed_out = (0..5).any(|_| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            drop(l); // nothing listens here now
            !probe_port_ready(port, Duration::from_millis(600))
        });
        assert!(timed_out, "probe must time out when nothing is listening");
    }

    #[test]
    fn probe_succeeds_when_port_open() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Spawn a thread to accept so the probe can actually connect.
        std::thread::spawn(move || {
            let _ = listener.accept();
        });
        let ok = probe_port_ready(port, Duration::from_secs(2));
        assert!(ok);
    }
}
