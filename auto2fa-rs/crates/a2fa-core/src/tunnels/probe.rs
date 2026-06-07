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
        match TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(500),
        ) {
            Ok(_) => return true,
            Err(_) => {}
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
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(!port_available(port)); // held by l
        drop(l);
        assert!(port_available(port)); // free now
    }

    #[test]
    fn probe_times_out_on_closed_port() {
        // Find a free port and do NOT bind it; the probe must timeout quickly.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l); // port is now free, nothing listens

        let ok = probe_port_ready(port, Duration::from_millis(600));
        assert!(!ok);
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
