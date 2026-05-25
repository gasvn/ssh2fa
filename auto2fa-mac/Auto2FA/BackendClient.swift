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

        var errorDescription: String? {
            switch self {
            case .notConnected: return "Not connected to auto2fa-daemon"
            case .decodeFailed(let s): return "Bad reply: \(s)"
            case .daemonError(_, let m): return m
            case .transport(let s): return s
            }
        }
    }

    // MARK: - State

    private var connection: NWConnection?
    private var receiveBuffer = Data()
    private var pendingRequests: [String: CheckedContinuation<Data, Error>] = [:]

    // Event stream — set up at init so callers can subscribe before connect()
    nonisolated let events: AsyncStream<DaemonEvent>
    private let eventContinuation: AsyncStream<DaemonEvent>.Continuation

    // True/false connection state pushes — AppState observes this to
    // re-bootstrap on reconnect and surface error banners on disconnect.
    nonisolated let connectionStates: AsyncStream<Bool>
    private let connectionStateCont: AsyncStream<Bool>.Continuation

    init() {
        var cont: AsyncStream<DaemonEvent>.Continuation!
        let stream = AsyncStream<DaemonEvent> { cont = $0 }
        self.events = stream
        self.eventContinuation = cont

        var stateCont: AsyncStream<Bool>.Continuation!
        let stateStream = AsyncStream<Bool> { stateCont = $0 }
        self.connectionStates = stateStream
        self.connectionStateCont = stateCont
    }

    // MARK: - Connect

    func connect() async throws {
        guard connection == nil else { return }
        let path = NWEndpoint.unix(path: BackendClient.socketPath)
        let conn = NWConnection(to: path, using: .tcp)
        connection = conn

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
                        Task { await self?.handleClosed() }
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
                        Task { await self?.handleClosed() }
                    }
                default:
                    break
                }
            }
            conn.start(queue: .global(qos: .userInitiated))
        }

        // Start the read loop AFTER connection is ready
        Task { await self.beginReceive() }
        // Subscribe to event pushes — non-fatal if it fails
        do { _ = try await sendRaw(method: "subscribe_events", params: [:]) }
        catch { /* swallow */ }
    }

    /// Replace the connect-time handler with one that only reacts to drops.
    private func installPostConnectHandler() {
        connection?.stateUpdateHandler = { [weak self] state in
            switch state {
            case .failed, .cancelled:
                Task { await self?.handleClosed() }
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

    /// Send a request, return the raw `result` JSON bytes (or throw).
    @discardableResult
    private func sendRaw(method: String, params: [String: Any]) async throws -> Data {
        guard let conn = connection else { throw ClientError.notConnected }
        let id = UUID().uuidString
        let payload: [String: Any] = ["id": id, "method": method, "params": params]
        var line = try JSONSerialization.data(withJSONObject: payload)
        line.append(0x0a)

        return try await withCheckedThrowingContinuation { cont in
            self.pendingRequests[id] = cont
            conn.send(content: line, completion: .contentProcessed { err in
                if let err {
                    Task { await self.failRequest(id: id, error: ClientError.transport(err.localizedDescription)) }
                }
            })
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
                Task { await self.handleIncoming(data) }
            }
            if isComplete || error != nil {
                Task { await self.handleClosed() }
                return
            }
            Task { await self.beginReceive() }
        }
    }

    private func handleIncoming(_ data: Data) {
        receiveBuffer.append(data)
        while let nl = receiveBuffer.firstIndex(of: 0x0a) {
            let line = receiveBuffer.subdata(in: receiveBuffer.startIndex..<nl)
            receiveBuffer.removeSubrange(receiveBuffer.startIndex...nl)
            guard !line.isEmpty else { continue }
            dispatch(line: line)
        }
    }

    private func handleClosed() {
        connection = nil
        for (_, c) in pendingRequests {
            c.resume(throwing: ClientError.notConnected)
        }
        pendingRequests.removeAll()
        // Notify subscribers we're down. AppState reacts by starting a
        // bounded reconnect-retry loop (with backoff) until ensureConnected
        // succeeds.
        connectionStateCont.yield(false)
    }

    /// Best-effort retry: tries connect() up to ~2 minutes with backoff.
    /// Yields true on success. Called by AppState on disconnect events.
    func reconnectWithBackoff() async {
        // 1, 2, 4, 8, 16, 30, 30, 30 …
        let delays: [UInt64] = [1, 2, 4, 8, 16, 30, 30, 30, 30, 30, 30, 30]
        for delay in delays {
            // Bail if user cancelled the bootstrap task entirely.
            if Task.isCancelled { return }
            try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
            do {
                try await connect()
                connectionStateCont.yield(true)
                return
            } catch {
                // keep trying
            }
        }
    }

    private func dispatch(line: Data) {
        guard let json = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else {
            return
        }
        // Event push (no id)
        if let eventName = json["event"] as? String {
            let dataDict = (json["data"] as? [String: Any]) ?? [:]
            let event = DaemonEvent.from(name: eventName, dict: dataDict)
            eventContinuation.yield(event)
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

    func setTunnelNode(_ name: String, node: String, user: String) async throws {
        _ = try await sendRaw(method: "tunnel_set_node",
                              params: ["name": name, "node": node, "user": user])
    }

    func discoverNodes(host: String) async throws -> [SqueueJob] {
        let data = try await sendRaw(method: "discover_nodes", params: ["host": host])
        return try JSONDecoder().decode([SqueueJob].self, from: data)
    }

    /// Notify daemon that the Mac just woke from sleep. Daemon will tear down
    /// every SSH master (their TCP is dead after suspend) and restart any
    /// tunnel that was alive at sleep time, after a ~20s grace window so the
    /// fresh masters have time to log back in.
    func wakeRecover() async throws {
        _ = try await sendRaw(method: "wake_recover", params: [:])
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
