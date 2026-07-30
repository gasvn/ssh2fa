import Foundation

/// Pure decision logic for recovering a wedged daemon connection.
///
/// Background: the app polls the daemon every 5s (`AppState.reloadAll`). A
/// healthy `list_hosts`/`list_tunnels` returns in <1ms, so a request that hits
/// its 10s timeout means the socket is dead. The trouble is a *silently*
/// half-open unix socket — the classic post-sleep case — where `NWConnection`
/// never fires `.failed`/close, so `handleClosed` (the only thing that yields
/// the "down" edge that drives `reconnectWithBackoff`) never runs. The poll
/// then times out forever and the app shows "Daemon is slow to respond" until
/// it is manually relaunched.
///
/// The poll is the reliable heartbeat, so this uses the consecutive-failure
/// streak to decide when to forcibly tear the dead socket down (which makes the
/// existing reconnect machinery run).
enum ConnectionRecovery {
    /// Consecutive `reloadAll` failures before we (a) surface the banner and
    /// (b) force-drop the socket. ~15s of a dead heartbeat — well past any
    /// single slow daemon op, since the polled methods are sub-millisecond.
    static let forceReconnectThreshold = 3

    /// Whether the banner ("Daemon is slow to respond — retrying…") should show.
    static func shouldShowSlowBanner(failStreak: Int) -> Bool {
        failStreak >= forceReconnectThreshold
    }

    /// Whether to force-drop the (silently-dead) socket so the connection
    /// watcher's reconnect loop runs.
    ///
    /// Fires at the crossing and then every `forceReconnectThreshold` failures
    /// (3, 6, 9, …) — NOT on every poll: a drop on every poll would keep
    /// cancelling the in-flight reconnect (whose `NWConnection` is assigned
    /// before its handshake completes), so it could never finish.
    ///
    /// It must RE-ARM rather than fire exactly once. `reconnectWithBackoff`
    /// gives up after ~4 minutes; with a single `== threshold` trigger, an
    /// outage longer than that budget left the app permanently disconnected —
    /// the streak kept climbing (4, 5, 6 …) so this never fired again, and the
    /// poll loop spun forever against a daemon that had long since come back.
    /// Observed 2026-07-29: ~1.5 h stuck on "Reconnecting to the background
    /// helper…" after a daemon update, fixed only by relaunching the app.
    /// Re-arming makes recovery unbounded; `BackendClient.isReconnecting`
    /// suppresses the redundant calls while an attempt is already running.
    static func shouldForceReconnect(failStreak: Int) -> Bool {
        failStreak >= forceReconnectThreshold
            && failStreak % forceReconnectThreshold == 0
    }
}
