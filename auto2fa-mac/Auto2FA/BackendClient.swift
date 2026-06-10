import Foundation
import Network

/// Thread-safe one-shot latch. Returns true exactly once; subsequent calls
/// return false. Used to ensure connect-time continuations resume exactly
/// once even though NWConnection.stateUpdateHandler can fire many times.
final class OneShot: @unchecked Sendable {
    private var fired = false
    private let lock = NSLock()
    func fire() -> Bool {
        lock.lock(); defer { lock.unlock() }
        if fired { return false }
        fired = true
        return true
    }
}

/// Async/await wrapper around the Python daemon's line-delimited JSON IPC
/// over `~/.auto2fa/auto2fa.sock`.
///
/// Each request is a JSON object terminated by `\n`. The daemon may at any
/// time push event objects (no `id` field) — these flow through `events`.
actor BackendClient {
    static let socketPath = ("~/.auto2fa/auto2fa.sock" as NSString).expandingTildeInPath

    enum ClientError: Error, LocalizedError {
        case notConnected
        case decodeFailed(String)
        case daemonError(code: String, message: String)
        case transport(String)
        case cancelled

        var errorDescription: String? {
            switch self {
            case .notConnected: return "Not connected to auto2fa-daemon"
            case .decodeFailed(let s): return "Bad reply: \(s)"
            case .daemonError(_, let m): return m
            case .transport(let s): return s
            case .cancelled: return "Request cancelled"
            }
        }
    }

    // MARK: - State

    private var connection: NWConnection?
    private var receiveBuffer = Data()
    private var pendingRequests: [String: CheckedContinuation<Data, Error>] = [:]

    // wake_recover coalescing (defense-in-depth; the daemon-side guard is
    // authoritative). SleepWakeMonitor and NetworkMonitor both fire on a single
    // wake, so without this two wake_recover RPCs go out back-to-back.
    private var wakeRecoverInFlight = false
    private var lastWakeRecoverAt: Date?
    private let wakeRecoverMinInterval: TimeInterval = 5.0

    // Event / connection-state fan-out.
    //
    // NOT single shared AsyncStreams: an AsyncStream is single-use — cancelling
    // the task iterating it FINISHES the stream (later yields are dropped and a
    // new `for await` returns immediately). AppState cancels + re-subscribes on
    // every reconnect, so with shared streams all pushed events (and disconnect
    // detection) silently died after the FIRST reconnect. Instead each
    // subscription gets a fresh stream; yields fan out to all live subscribers
    // and onTermination prunes just that subscriber.
    private var eventSubscribers: [UUID: AsyncStream<DaemonEvent>.Continuation] = [:]
    private var stateSubscribers: [UUID: AsyncStream<Bool>.Continuation] = [:]

    /// A fresh per-subscriber event stream (safe to drop/re-call on reconnect).
    func eventStream() -> AsyncStream<DaemonEvent> {
        let id = UUID()
        return AsyncStream { cont in
            self.eventSubscribers[id] = cont
            cont.onTermination = { @Sendable _ in
                Task { await self.removeEventSubscriber(id) }
            }
        }
    }

    /// A fresh per-subscriber connection-state stream.
    func connectionStateStream() -> AsyncStream<Bool> {
        let id = UUID()
        return AsyncStream { cont in
            self.stateSubscribers[id] = cont
            cont.onTermination = { @Sendable _ in
                Task { await self.removeStateSubscriber(id) }
            }
        }
    }

    private func removeEventSubscriber(_ id: UUID) { eventSubscribers[id] = nil }
    private func removeStateSubscriber(_ id: UUID) { stateSubscribers[id] = nil }
    private func yieldEvent(_ e: DaemonEvent) {
        for c in eventSubscribers.values { c.yield(e) }
    }
    private func yieldConnectionState(_ up: Bool) {
        for c in stateSubscribers.values { c.yield(up) }
    }

    init() {}

    // MARK: - Connect

    func connect() async throws {
        guard connection == nil else { return }
        // A partial line left over from the OLD connection would prepend
        // itself to the first response on the new one (corrupting it and
        // burning that request's timeout) — start clean.
        receiveBuffer.removeAll()
        let path = NWEndpoint.unix(path: BackendClient.socketPath)
        let conn = NWConnection(to: path, using: .tcp)
        connection = conn

        do {
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
                // stateUpdateHandler fires on every state change; the connect
                // continuation must be resumed exactly once, then the handler
                // swapped to one that only logs/handles drops post-connect.
                let resumed = OneShot()
                conn.stateUpdateHandler = { [weak self] state in
                    switch state {
                    case .ready:
                        if resumed.fire() {
                            cont.resume()
                            Task { await self?.installPostConnectHandler() }
                        }
                    case .failed(let err):
                        if resumed.fire() {
                            cont.resume(throwing: ClientError.transport(err.localizedDescription))
                        } else {
                            Task { await self?.handleClosed(conn) }
                        }
                    case .waiting(let err):
                        // For a unix socket, .waiting usually means the socket file
                        // doesn't exist (daemon not running). Treat as fatal at
                        // connect time so we don't hang.
                        if resumed.fire() {
                            cont.resume(throwing: ClientError.transport(err.localizedDescription))
                        }
                    case .cancelled:
                        if !resumed.fire() {
                            Task { await self?.handleClosed(conn) }
                        }
                    default:
                        break
                    }
                }
                conn.start(queue: .global(qos: .userInitiated))
            }
        } catch {
            // A connect-time failure (.failed/.waiting) resumed the
            // continuation by throwing, but `connection` was assigned BEFORE
            // the handshake (line above). If we leave it set, the next
            // connect() short-circuits on `guard connection == nil` and
            // returns WITHOUT throwing — which reconnectWithBackoff reads as
            // success, wedging the app in a fake "connected" state where every
            // request fails. Clear the dead connection so the next attempt
            // genuinely reconnects.
            conn.cancel()
            if connection === conn { connection = nil }
            throw error
        }

        // Start the read loop AFTER connection is ready
        Task { await self.beginReceive() }
        // Subscribe to event pushes — non-fatal if it fails
        do { _ = try await sendRaw(method: "subscribe_events", params: [:]) }
        catch { /* swallow */ }
    }

    /// Replace the connect-time handler with one that only reacts to drops.
    private func installPostConnectHandler() {
        guard let conn = connection else { return }
        conn.stateUpdateHandler = { [weak self] state in
            switch state {
            case .failed, .cancelled:
                Task { await self?.handleClosed(conn) }
            default:
                break
            }
        }
    }

    func disconnect() {
        connection?.cancel()
        connection = nil
        for (_, c) in pendingRequests {
            c.resume(throwing: ClientError.notConnected)
        }
        pendingRequests.removeAll()
    }

    // MARK: - Request / receive

    /// Per-method timeout defaults. Most methods are listing/setter
    /// operations that finish in <1s — but a few legitimately take longer
    /// (set_node triggers a 10s port-probe, wake_recover scans masters,
    /// host_test_credentials does a fresh login). 30-45s is a generous
    /// upper bound that still catches a wedged daemon.
    private static func defaultTimeout(for method: String) -> TimeInterval {
        switch method {
        case "tunnel_set_node", "tunnel_toggle",
             "tunnel_start", "tunnel_rename",
             "tunnels_batch", "tunnel_set_jump_candidates",
             "wake_recover", "reset_all":
            return 30
        case "host_mount_toggle":
            // sshfs mount path worst case: `which` 5s + sshfs 45s +
            // reap_failed_sshfs ~15s, all inline in the daemon. A 30s client
            // timeout reported "timed out" for mounts that then SUCCEEDED
            // ~30s later (row flips with no explanation, retry hits
            // "already in progress").
            return 75
        case "host_test_credentials":
            // A fresh pty login is bounded daemon-side at 60s PLUS the OTP
            // replay-guard's next-window wait (~+30s). 45s fired while real
            // tests were still legitimately running.
            return 100
        case "host_add":
            return 45
        case "discover_nodes":
            // squeue over the master socket has a 15s daemon-side deadline;
            // the old 10s default failed the node picker while the job list
            // was about to arrive.
            return 20
        case "host_totp":
            // Short timeout: the chip should fall back to its muted state fast
            // rather than hang if a Keychain "Always Allow" prompt is pending.
            return 6
        default:
            return 10
        }
    }

    /// Send a request, return the raw `result` JSON bytes (or throw).
    /// Cancellation-aware AND timeout-aware. Without a timeout a hung
    /// daemon would leave the caller awaiting forever — every Mac-side
    /// await would pile up indefinitely. We resume the pending
    /// continuation with .transport("daemon timeout") if the configured
    /// per-method deadline is exceeded.
    @discardableResult
    private func sendRaw(method: String, params: [String: Any]) async throws -> Data {
        guard let conn = connection else { throw ClientError.notConnected }
        let id = UUID().uuidString
        let payload: [String: Any] = ["id": id, "method": method, "params": params]
        var line = try JSONSerialization.data(withJSONObject: payload)
        line.append(0x0a)

        let timeoutSec = BackendClient.defaultTimeout(for: method)
        let timeoutTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(timeoutSec * 1_000_000_000))
            if Task.isCancelled { return }
            await self?.failRequest(id: id,
                error: ClientError.transport("daemon timed out after \(Int(timeoutSec))s on \(method)"))
        }

        return try await withTaskCancellationHandler {
            do {
                let data = try await withCheckedThrowingContinuation { cont in
                    self.pendingRequests[id] = cont
                    conn.send(content: line, completion: .contentProcessed { err in
                        if let err {
                            Task { await self.failRequest(id: id,
                                error: ClientError.transport(err.localizedDescription)) }
                        }
                    })
                }
                timeoutTask.cancel()
                return data
            } catch {
                timeoutTask.cancel()
                throw error
            }
        } onCancel: {
            timeoutTask.cancel()
            Task { await self.failRequest(id: id, error: ClientError.cancelled) }
        }
    }

    private func failRequest(id: String, error: Error) {
        if let cont = pendingRequests.removeValue(forKey: id) {
            cont.resume(throwing: error)
        }
    }

    private func beginReceive() {
        guard let conn = connection else { return }
        conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                Task { await self.handleIncoming(data, from: conn) }
            }
            if isComplete || error != nil {
                Task { await self.handleClosed(conn) }
                return
            }
            Task { await self.beginReceive() }
        }
    }

    private func handleIncoming(_ data: Data, from conn: NWConnection) {
        // A late callback from a superseded connection must not pollute the
        // new connection's line buffer.
        guard conn === connection else { return }
        receiveBuffer.append(data)
        while let nl = receiveBuffer.firstIndex(of: 0x0a) {
            let line = receiveBuffer.subdata(in: receiveBuffer.startIndex..<nl)
            receiveBuffer.removeSubrange(receiveBuffer.startIndex...nl)
            guard !line.isEmpty else { continue }
            dispatch(line: line)
        }
    }

    private func handleClosed(_ closed: NWConnection?) {
        // Identity guard: NW callbacks from the OLD connection can land AFTER
        // a successful reconnect — without this check they nil'd the NEW
        // connection, failed its pending requests, and yielded a spurious
        // "down". Only the CURRENT connection's close is real.
        if let closed, let current = connection, closed !== current { return }
        // Cancel the dropped connection so its fd doesn't linger to dealloc.
        (closed ?? connection)?.cancel()
        connection = nil
        for (_, c) in pendingRequests {
            c.resume(throwing: ClientError.notConnected)
        }
        pendingRequests.removeAll()
        // Notify subscribers we're down. AppState reacts by starting a
        // bounded reconnect-retry loop (with backoff) until ensureConnected
        // succeeds.
        yieldConnectionState(false)
    }

    /// Best-effort retry: tries connect() up to ~2 minutes with backoff.
    /// Yields true and returns true on success. Returns false if every
    /// attempt failed (or the task was cancelled) so the caller can surface a
    /// terminal "couldn't reconnect" state instead of leaving the user on a
    /// "retrying…" banner forever.
    @discardableResult
    func reconnectWithBackoff() async -> Bool {
        // 1, 2, 4, 8, 16, 30, 30, 30 …
        let delays: [UInt64] = [1, 2, 4, 8, 16, 30, 30, 30, 30, 30, 30, 30]
        for delay in delays {
            // Bail if user cancelled the bootstrap task entirely.
            if Task.isCancelled { return false }
            try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
            do {
                try await connect()
                yieldConnectionState(true)
                return true
            } catch {
                // keep trying
            }
        }
        return false
    }

    private func dispatch(line: Data) {
        guard let json = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else {
            return
        }
        // Event push (no id)
        if let eventName = json["event"] as? String {
            let dataDict = (json["data"] as? [String: Any]) ?? [:]
            let event = DaemonEvent.from(name: eventName, dict: dataDict)
            yieldEvent(event)
            return
        }
        // Response
        guard let id = json["id"] as? String,
              let cont = pendingRequests.removeValue(forKey: id) else { return }
        if let err = json["error"] as? [String: Any] {
            let code = err["code"] as? String ?? "unknown"
            let msg = err["message"] as? String ?? ""
            cont.resume(throwing: ClientError.daemonError(code: code, message: msg))
        } else {
            // Re-serialize the result for the caller to decode into its concrete type
            let resultAny = json["result"] ?? NSNull()
            do {
                let data = try JSONSerialization.data(withJSONObject: resultAny,
                                                       options: [.fragmentsAllowed])
                cont.resume(returning: data)
            } catch {
                cont.resume(throwing: ClientError.decodeFailed(String(describing: error)))
            }
        }
    }

    // MARK: - Typed convenience methods

    func listHosts() async throws -> [SSHHost] {
        let data = try await sendRaw(method: "list_hosts", params: [:])
        return try JSONDecoder().decode([SSHHost].self, from: data)
    }

    /// Live TOTP code for a host. Returns ONLY the 6-digit code (never the
    /// secret) plus the period and seconds remaining in the current window.
    struct TOTPCode: Decodable {
        let code: String
        let period: Int
        let seconds_remaining: Int
    }
    func hostTOTP(_ host: String) async throws -> TOTPCode {
        let data = try await sendRaw(method: "host_totp", params: ["host": host])
        return try JSONDecoder().decode(TOTPCode.self, from: data)
    }

    func listTunnels() async throws -> [Tunnel] {
        let data = try await sendRaw(method: "list_tunnels", params: [:])
        return try JSONDecoder().decode([Tunnel].self, from: data)
    }

    func toggleHost(_ host: String) async throws {
        _ = try await sendRaw(method: "host_toggle", params: ["host": host])
    }

    func toggleMount(_ host: String) async throws {
        _ = try await sendRaw(method: "host_mount_toggle", params: ["host": host])
    }

    func rotateHost(_ host: String) async throws {
        _ = try await sendRaw(method: "host_rotate", params: ["host": host])
    }

    func addTunnel(name: String, localPort: Int, remotePort: Int? = nil) async throws -> Tunnel {
        var params: [String: Any] = ["name": name, "local_port": localPort]
        if let rp = remotePort { params["remote_port"] = rp }
        let data = try await sendRaw(method: "tunnel_add", params: params)
        return try JSONDecoder().decode(Tunnel.self, from: data)
    }

    func removeTunnel(_ name: String) async throws {
        _ = try await sendRaw(method: "tunnel_remove", params: ["name": name])
    }

    func toggleTunnel(_ name: String) async throws {
        _ = try await sendRaw(method: "tunnel_toggle", params: ["name": name])
    }

    func setTunnelNode(_ name: String, node: String, user: String,
                       start: Bool = true) async throws {
        var params: [String: Any] = ["name": name, "node": node, "user": user]
        // Only send `start` when false — older daemons ignore the unknown
        // key, and the daemon default is true anyway.
        if !start { params["start"] = false }
        _ = try await sendRaw(method: "tunnel_set_node", params: params)
    }

    func discoverNodes(host: String) async throws -> [SqueueJob] {
        let data = try await sendRaw(method: "discover_nodes", params: ["host": host])
        return try JSONDecoder().decode([SqueueJob].self, from: data)
    }

    /// Notify daemon that the Mac just woke from sleep. Daemon will tear down
    /// every SSH master (their TCP is dead after suspend) and restart any
    /// tunnel that was alive at sleep time, after a ~20s grace window so the
    /// fresh masters have time to log back in.
    /// Returns `true` iff the daemon actually RAN a recovery pass —
    /// `false` when coalesced (client-side guards or the daemon's own
    /// in-flight/debounce guard). Callers use this to avoid toasting
    /// activity that didn't happen (one wake fires BOTH Mac monitors).
    @discardableResult
    func wakeRecover() async throws -> Bool {
        // Coalesce: if a wake_recover is already in flight, or one completed
        // within the last few seconds, skip — the two Mac monitors fire on a
        // single wake. Actor isolation makes this check/set atomic.
        if wakeRecoverInFlight {
            NSLog("[Auto2FA] wakeRecover already in flight — coalescing")
            return false
        }
        if let last = lastWakeRecoverAt,
           Date().timeIntervalSince(last) < wakeRecoverMinInterval {
            NSLog("[Auto2FA] wakeRecover ran <\(Int(wakeRecoverMinInterval))s ago — coalescing")
            return false
        }
        wakeRecoverInFlight = true
        defer {
            wakeRecoverInFlight = false
            lastWakeRecoverAt = Date()
        }
        let data = try await sendRaw(method: "wake_recover", params: [:])
        if let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let coalesced = obj["coalesced"] as? Bool {
            return !coalesced
        }
        return true
    }

    func setTunnelAutostart(_ name: String, value: Bool) async throws {
        _ = try await sendRaw(method: "tunnel_set_autostart",
                              params: ["name": name, "value": value])
    }

    /// Set the per-tunnel jump-host whitelist. nil = auto (any ready host),
    /// non-nil = priority-ordered list (first ready one wins). Daemon
    /// restarts the tunnel immediately if it's currently alive.
    func setTunnelJumpCandidates(_ name: String, candidates: [String]?) async throws {
        var params: [String: Any] = ["name": name]
        // JSONSerialization writes NSNull as JSON null.
        params["candidates"] = candidates as Any? ?? NSNull()
        _ = try await sendRaw(method: "tunnel_set_jump_candidates", params: params)
    }

    /// Ask the daemon for the next free local port, starting at `base`.
    func suggestPort(base: Int = 8888) async throws -> Int {
        let data = try await sendRaw(method: "port_suggest", params: ["base": base])
        struct R: Decodable { let port: Int }
        return try JSONDecoder().decode(R.self, from: data).port
    }

    /// Verify the supplied credentials against the real server WITHOUT
    /// persisting them. Returns (ok, reason). Use this from the Add Host
    /// wizard to refuse a save when password/OTP are wrong — otherwise the
    /// daemon's auto-retry loop produces dozens of failed-login attempts
    /// which trigger server-side rate-limiting.
    func testHostCredentials(host: String, password: String,
                             otpauthURL: String) async throws -> (Bool, String) {
        let data = try await sendRaw(method: "host_test_credentials", params: [
            "host": host, "password": password, "otpauth_url": otpauthURL,
        ])
        struct R: Decodable { let ok: Bool; let reason: String }
        let r = try JSONDecoder().decode(R.self, from: data)
        return (r.ok, r.reason)
    }

    func addHost(host: String, password: String, otpauthURL: String,
                 autoConnect: Bool) async throws -> SSHHost {
        let data = try await sendRaw(method: "host_add", params: [
            "host": host,
            "password": password,
            "otpauth_url": otpauthURL,
            "auto_connect": autoConnect,
        ])
        return try JSONDecoder().decode(SSHHost.self, from: data)
    }

    func setTunnelTags(_ name: String, tags: [String]) async throws {
        _ = try await sendRaw(method: "tunnel_set_tags",
                              params: ["name": name, "tags": tags])
    }

    func setTunnelUrlPath(_ name: String, path: String?) async throws {
        var params: [String: Any] = ["name": name]
        params["path"] = path as Any? ?? NSNull()
        _ = try await sendRaw(method: "tunnel_set_url_path", params: params)
    }

    func renameTunnel(old: String, new: String) async throws {
        _ = try await sendRaw(method: "tunnel_rename",
                              params: ["old": old, "new": new])
    }

    struct BatchResult: Decodable {
        let name: String
        let ok: Bool
        let error: String?
    }
    func batchTunnels(action: String, names: [String]) async throws -> [BatchResult] {
        let data = try await sendRaw(method: "tunnels_batch",
                                     params: ["action": action, "names": names])
        struct R: Decodable { let results: [BatchResult] }
        return try JSONDecoder().decode(R.self, from: data).results
    }

    /// Per-tunnel activity log (ring buffer, latest 200 events on daemon
    /// side). Each event is {ts: Double (epoch s), msg: String}.
    struct TunnelEvent: Codable, Identifiable, Hashable {
        let ts: Double
        let msg: String
        var id: String { "\(ts)-\(msg)" }
        var date: Date { Date(timeIntervalSince1970: ts) }
    }
    func tunnelEvents(_ name: String) async throws -> [TunnelEvent] {
        let data = try await sendRaw(method: "tunnel_events", params: ["name": name])
        struct R: Decodable { let events: [TunnelEvent] }
        return try JSONDecoder().decode(R.self, from: data).events
    }

    /// Set / clear the post-connect shell command for a tunnel. Pass nil
    /// or "" to clear.
    func setTunnelPostConnect(_ name: String, cmd: String?) async throws {
        var params: [String: Any] = ["name": name]
        params["cmd"] = cmd as Any? ?? NSNull()
        _ = try await sendRaw(method: "tunnel_set_post_connect", params: params)
    }

    /// Nuclear "reset everything" — stops every tunnel + rebuilds every
    /// master. Returns counts so callers can toast a confirmation.
    func resetAll() async throws -> (tunnelsStopped: Int, mastersRebuilt: Int) {
        let data = try await sendRaw(method: "reset_all", params: [:])
        struct R: Decodable {
            let tunnels_stopped: Int
            let masters_rebuilt: Int
        }
        let r = try JSONDecoder().decode(R.self, from: data)
        return (r.tunnels_stopped, r.masters_rebuilt)
    }

    func logTail(lines: Int = 200) async throws -> [String] {
        let data = try await sendRaw(method: "log_tail", params: ["lines": lines])
        struct R: Decodable { let lines: [String] }
        return try JSONDecoder().decode(R.self, from: data).lines
    }
}

// MARK: - Event types

/// Sendable, typed event stream. Each case carries its decoded payload.
enum DaemonEvent: Sendable {
    case hostChanged(host: String, status: String, isMasterReady: Bool, lastMsg: String)
    case tunnelChanged(name: String, status: String, lastMsg: String, activeJump: String?)
    case notification(severity: String, title: String, message: String)
    case unknown(name: String)

    static func from(name: String, dict: [String: Any]) -> DaemonEvent {
        switch name {
        case "host_status_changed":
            return .hostChanged(
                host: dict["host"] as? String ?? "",
                status: dict["status"] as? String ?? "",
                isMasterReady: dict["is_master_ready"] as? Bool ?? false,
                lastMsg: dict["last_msg"] as? String ?? ""
            )
        case "tunnel_status_changed":
            return .tunnelChanged(
                name: dict["name"] as? String ?? "",
                status: dict["status"] as? String ?? "",
                lastMsg: dict["last_msg"] as? String ?? "",
                activeJump: dict["active_jump"] as? String
            )
        case "notification":
            return .notification(
                severity: dict["severity"] as? String ?? "info",
                title: dict["title"] as? String ?? "",
                message: dict["message"] as? String ?? ""
            )
        default:
            return .unknown(name: name)
        }
    }
}
