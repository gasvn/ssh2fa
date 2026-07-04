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
    /// Fires EXACTLY at the crossing, not on every later poll: a repeated drop
    /// would keep cancelling the in-flight reconnect (whose `NWConnection` is
    /// assigned before its handshake completes), so it could never finish.
    /// One drop per down-episode is enough — the streak resets to 0 on the next
    /// success, so a subsequent death re-arms it.
    static func shouldForceReconnect(failStreak: Int) -> Bool {
        failStreak == forceReconnectThreshold
    }
}
