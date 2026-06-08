import Foundation
import SwiftUI

/// Observable mirror of daemon state. Lives for the lifetime of the app.
///
/// Owns one `BackendClient`. Periodically pulls full snapshots AND reacts to
/// pushed events for instant updates. Falls back to polling if the daemon
/// hasn't pushed an event in a while.
/// Which modal sheet (if any) the main window is showing.
enum ActiveSheet: Identifiable, Equatable {
    case newTunnel
    case nodePicker(tunnelName: String)
    case customNode(tunnelName: String)
    case confirmDelete(tunnelName: String)
    case addHost

    var id: String {
        switch self {
        case .newTunnel: return "newTunnel"
        case .nodePicker(let n): return "nodePicker:\(n)"
        case .customNode(let n): return "customNode:\(n)"
        case .confirmDelete(let n): return "confirmDelete:\(n)"
        case .addHost: return "addHost"
        }
    }
}

@MainActor
final class AppState: ObservableObject {
    @Published var hosts: [SSHHost] = []
    @Published var tunnels: [Tunnel] = []
    @Published var connectionError: String?
    @Published var notchPresenter: NotchPresenter = NotchPresenter()
    let persistentNotch: PersistentNotchController = PersistentNotchController()
    @Published var activeSheet: ActiveSheet?
    /// Names of hosts/tunnels with an action currently in flight (toggle,
    /// pick_node, delete). UI uses this to swap the action button for a
    /// spinner and overlay a "Working…" status so the user sees that their
    /// click was received — daemon-side operations can take 10-30s while
    /// they probe the local port / wait for SSH to settle.
    @Published var inFlightHosts: Set<String> = []
    @Published var inFlightTunnels: Set<String> = []
    /// Snapshot of the most recently deleted tunnel, kept ~8s so the user
    /// can hit Undo from the snackbar. Auto-clears on timer or on the next
    /// successful delete.
    @Published var undoableDelete: Tunnel?
    private var undoExpireTask: Task<Void, Never>?

    let client = BackendClient()
    private var eventTask: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?

    func bootstrap() async {
        NSLog("[Auto2FA] bootstrap: connecting to daemon")
        do {
            try await client.connect()
            connectionError = nil
            NSLog("[Auto2FA] bootstrap: connected OK")
            notchPresenter.show(
                systemImage: "bolt.fill",
                title: "Auto2FA ready",
                description: "Connected to daemon",
                tint: .green
            )
        } catch {
            NSLog("[Auto2FA] bootstrap: connect failed: \(error.localizedDescription)")
            connectionError = "Daemon unreachable: \(error.localizedDescription). " +
                              "Is auto2fa-daemon running?"
            return
        }
        await reloadAll()
        NSLog("[Auto2FA] bootstrap: loaded \(hosts.count) hosts, \(tunnels.count) tunnels")
        startEventTask()
        startConnectionWatcher()
        startPollFallback()
    }

    /// Listen for daemon disconnect / reconnect cycles. On disconnect we
    /// surface a banner + show a notch toast and kick off a backoff retry
    /// in a SEPARATE Task — otherwise the watcher loop blocks for the
    /// full backoff window (up to ~2 minutes) and the `true` yielded on
    /// reconnect arrives but isn't consumed until then.
    private var connWatcherTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private func startConnectionWatcher() {
        connWatcherTask?.cancel()
        let stream = client.connectionStates
        connWatcherTask = Task { [weak self] in
            for await connected in stream {
                guard let self else { return }
                if connected {
                    await MainActor.run {
                        self.connectionError = nil
                        self.notchPresenter.show(
                            systemImage: "bolt.fill",
                            title: "Daemon reconnected",
                            description: "state restored",
                            tint: .green
                        )
                    }
                    await self.reloadAll()
                    self.startEventTask()  // re-subscribe events on the new socket
                } else {
                    await MainActor.run {
                        self.connectionError = "Daemon disconnected — retrying…"
                        self.notchPresenter.show(
                            systemImage: "wifi.slash",
                            title: "Daemon lost",
                            description: "auto-reconnecting…",
                            tint: .orange
                        )
                    }
                    // Run reconnect detached so the watcher loop keeps
                    // pulling state changes from the stream.
                    self.reconnectTask?.cancel()
                    self.reconnectTask = Task { [weak self] in
                        guard let self else { return }
                        // First: if we OWNED the daemon process and it's
                        // dead (not just the socket), respawn before
                        // hammering the socket with retries that can
                        // never succeed.
                        //
                        // Loop the respawn with backoff — previously a single
                        // failed respawn left the app in a permanently-dead
                        // "Daemon respawn failed" state until manual restart.
                        let delays: [UInt64] = [2, 5, 10, 30, 60, 60, 60]
                        for delay in delays {
                            if Task.isCancelled { return }
                            if let respawn = await DaemonProcess.shared.respawnIfOwnedDaemonCrashed() {
                                switch respawn {
                                case .alreadyRunning, .spawned:
                                    NSLog("[Auto2FA] daemon respawned after crash")
                                    await self.bootstrap()
                                    return
                                case .failed(let reason):
                                    NSLog("[Auto2FA] daemon respawn failed: \(reason), retrying")
                                    await MainActor.run {
                                        self.connectionError = "Daemon respawn failed (will retry): \(reason)"
                                    }
                                    try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                                    continue
                                }
                            }
                            // We don't own a daemon — fall back to socket-
                            // level reconnect (LaunchAgent / external daemon).
                            break
                        }
                        // If every backoff attempt failed, say so plainly
                        // instead of leaving the "retrying…" banner up forever.
                        // (On success reconnectWithBackoff yields true, which
                        // the watcher turns into connectionError = nil.)
                        let ok = await self.client.reconnectWithBackoff()
                        if !ok && !Task.isCancelled {
                            await MainActor.run {
                                self.connectionError =
                                    "Couldn't reconnect to the daemon. Restart Auto2FA, or check /tmp/auto2fa_daemon.log."
                            }
                        }
                    }
                }
            }
        }
    }

    func reloadAll() async {
        let isFirstLoad = self.tunnels.isEmpty && lastNotchSignature.isEmpty
        do {
            self.hosts = try await client.listHosts()
            self.tunnels = try await client.listTunnels()
            updateDockBadge()
            // On the very first reload at app launch, seed the dedup map
            // with every tunnel's current status — otherwise the first
            // batch of TUNNEL_STATUS_CHANGED events would each be treated
            // as "new alive transition" and we'd fire N "Connected X"
            // notches in rapid succession for tunnels that have been
            // alive for hours.
            if isFirstLoad {
                for t in self.tunnels {
                    self.lastNotchSignature[t.name] = t.status
                }
            }
        } catch {
            connectionError = error.localizedDescription
        }
    }

    func reloadHostsOnly() async {
        do {
            self.hosts = try await client.listHosts()
            updateDockBadge()
        } catch { connectionError = error.localizedDescription }
    }

    func reloadTunnelsOnly() async {
        do {
            self.tunnels = try await client.listTunnels()
            updateDockBadge()
            // Clean stale dedup entries for tunnels that no longer exist
            // (renamed, deleted) so the dict doesn't grow forever AND so
            // a future tunnel re-using an old name gets a real first notch.
            let liveNames = Set(self.tunnels.map(\.name))
            self.lastNotchSignature = self.lastNotchSignature.filter { liveNames.contains($0.key) }
        } catch { connectionError = error.localizedDescription }
    }

    /// Set the Dock-tile badge to the # of alive tunnels (or to the # of
    /// failed things prefixed with "!"). Fires whenever state reloads.
    /// Also drives the persistent notch overlay (off by default).
    private func updateDockBadge() {
        var alive = 0
        var failed = 0
        for t in tunnels {
            switch t.displayState {
            case .alive: alive += 1
            case .failed, .portBusy: failed += 1
            default: break
            }
        }
        for h in hosts where h.displayState == .failed {
            failed += 1
        }
        let label: String?
        if failed > 0 { label = "!\(failed)" }
        else if alive > 0 { label = "\(alive)" }
        else { label = nil }
        NSApp.dockTile.badgeLabel = label
        // Refresh persistent notch (cheap — early-outs if signature unchanged).
        persistentNotch.update(from: self)
    }

    private func startEventTask() {
        eventTask?.cancel()
        let stream = client.events
        eventTask = Task { [weak self] in
            for await event in stream {
                guard let self else { return }
                await self.apply(event: event)
            }
        }
    }

    private func startPollFallback() {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000) // 5s safety net
                guard let self else { return }
                // Don't spam the daemon with reloadAll while we know it's
                // disconnected — reconnectWithBackoff is already retrying
                // and reloadAll's failure would just overwrite the
                // connectionError banner with the same message each tick.
                if self.connectionError == nil {
                    await self.reloadAll()
                }
            }
        }
    }

    private func apply(event: DaemonEvent) async {
        switch event {
        case .hostChanged:
            // Daemon's host event doesn't carry the full snapshot, so we
            // refetch hosts only (NOT tunnels — that used to thrash the UI
            // on every host heartbeat tick).
            await reloadHostsOnly()
        case .tunnelChanged(let name, let status, let lastMsg, _):
            let prev = tunnels.first(where: { $0.name == name })
            let wasAlive: Bool = (prev?.displayState == Tunnel.DisplayState.alive)
            // Only reload tunnels (not hosts) on a tunnel event.
            await reloadTunnelsOnly()
            maybeShowNotch(name: name, status: status, lastMsg: lastMsg)
            if status == "alive" && !wasAlive {
                if let t = tunnels.first(where: { $0.name == name }) {
                    maybeAutoOpenBrowser(for: t)
                }
            }
            // Hand-off: post a macOS notification on hard failures with
            // action buttons (Restart / Show Activity) so user can react
            // without switching back to the app.
            if status == "failed" || status == "stale" {
                MacNotifications.postTunnelFailed(
                    name: name,
                    body: lastMsg.isEmpty ? "see app for details" : lastMsg
                )
            }
        case .notification(let severity, let title, let message):
            notchPresenter.show(
                systemImage: severity == "error" ? "exclamationmark.octagon.fill"
                          : severity == "warning" ? "exclamationmark.triangle.fill"
                          : "info.circle.fill",
                title: title,
                description: message,
                tint: severity == "error" ? .red : severity == "warning" ? .orange : .blue
            )
        case .unknown:
            break
        }
    }

    /// Honour the "Open URL in browser on tunnel up" setting. Fires from
    /// apply(event:) once per idle/starting → alive transition.
    /// Uses browserURL so per-tunnel url_path suffix (e.g. jupyter token)
    /// is appended automatically.
    private func maybeAutoOpenBrowser(for t: Tunnel) {
        guard UserDefaults.standard.bool(forKey: "auto2fa.autoOpenBrowser") else { return }
        if let url = URL(string: t.browserURL) {
            NSWorkspace.shared.open(url)
        }
    }

    /// Names+statuses we last toasted, to avoid spamming the notch when a
    /// daemon-side change-detector mistakenly fires the same status over and
    /// over. (Belt-and-suspenders — the daemon's _tunnel_change_key handles
    /// the real fix, this just prevents any future regression from drowning
    /// the user in notches.)
    private var lastNotchSignature: [String: String] = [:]

    private func maybeShowNotch(name: String, status: String, lastMsg: String) {
        if UserDefaults.standard.object(forKey: "auto2fa.notch.enabled") != nil,
           UserDefaults.standard.bool(forKey: "auto2fa.notch.enabled") == false {
            return
        }
        // Dedup: if the last notch we showed for this tunnel had the same
        // status string, skip. This makes "Connected" fire only on a real
        // idle/starting → alive transition, never on repeat snapshots.
        if lastNotchSignature[name] == status { return }
        lastNotchSignature[name] = status
        switch status {
        case "alive":
            notchPresenter.show(
                systemImage: "bolt.fill",
                title: "Connected",
                description: name,
                tint: .green
            )
        case "failed", "stale":
            notchPresenter.show(
                systemImage: "exclamationmark.triangle.fill",
                title: status == "failed" ? "Disconnected" : "Node ended",
                description: "\(name): \(lastMsg)",
                tint: .red
            )
        case "starting":
            notchPresenter.show(
                systemImage: "arrow.triangle.2.circlepath",
                title: "Connecting…",
                description: name,
                tint: .yellow
            )
        default:
            break
        }
    }

    // MARK: - User actions (thin wrappers that report errors via connectionError)

    func toggleHost(_ host: SSHHost) async {
        inFlightHosts.insert(host.host)
        defer { inFlightHosts.remove(host.host) }
        // Immediate notch so the user sees their click landed.
        notchPresenter.show(
            // Verb is based on whether the host is currently ON (active), not on
            // whether it's fully connected — so stopping a host stuck
            // reconnecting correctly shows "Stopping", not "Starting".
            systemImage: host.active ? "stop.fill" : "arrow.triangle.2.circlepath",
            title: host.active ? "Stopping" : "Starting",
            description: host.host,
            tint: .yellow
        )
        do { try await client.toggleHost(host.host) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    func toggleTunnel(_ tunnel: Tunnel) async {
        inFlightTunnels.insert(tunnel.name)
        defer { inFlightTunnels.remove(tunnel.name) }
        notchPresenter.show(
            systemImage: (tunnel.displayState == .alive || tunnel.displayState == .starting) ? "stop.fill" : "arrow.triangle.2.circlepath",
            title: (tunnel.displayState == .alive || tunnel.displayState == .starting) ? "Stopping" : "Starting",
            description: tunnel.name,
            tint: .yellow
        )
        do { try await client.toggleTunnel(tunnel.name) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    func deleteTunnel(_ tunnel: Tunnel) async {
        inFlightTunnels.insert(tunnel.name)
        defer { inFlightTunnels.remove(tunnel.name) }
        do { try await client.removeTunnel(tunnel.name) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
        // Stash a snapshot so the snackbar can offer Undo for ~8s.
        undoableDelete = tunnel
        undoExpireTask?.cancel()
        undoExpireTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 8_000_000_000)
            guard let self else { return }
            await MainActor.run {
                if self.undoableDelete?.name == tunnel.name {
                    self.undoableDelete = nil
                }
            }
        }
    }

    /// Re-create a tunnel from a snapshot. Used by the Undo snackbar after
    /// a delete. We re-issue the addTunnel + restore the persistent fields
    /// (auto_start, post_connect_cmd, tags, jump_candidates, last_node).
    ///
    /// IMPORTANT: tunnel_set_node has a side effect of STARTING the tunnel
    /// on the daemon side. So we only call it if the tunnel was alive at
    /// delete time — restoring an idle tunnel that just happens to have a
    /// `lastNode` from a previous run would otherwise unexpectedly start
    /// it. If you want a faithful restore that doesn't kick the tunnel,
    /// the daemon would need a `set_node_no_start` flavor; for now the
    /// approximation is "was alive → keep it alive; was idle → leave idle".
    func undoDelete() async {
        guard let t = undoableDelete else { return }
        undoableDelete = nil
        undoExpireTask?.cancel()
        do {
            _ = try await client.addTunnel(name: t.name, localPort: t.localPort)
            if t.autoStart {
                try? await client.setTunnelAutostart(t.name, value: true)
            }
            if !t.tags.isEmpty {
                try? await client.setTunnelTags(t.name, tags: t.tags)
            }
            if let cmd = t.postConnectCmd, !cmd.isEmpty {
                try? await client.setTunnelPostConnect(t.name, cmd: cmd)
            }
            if let jc = t.jumpCandidates {
                try? await client.setTunnelJumpCandidates(t.name, candidates: jc)
            }
            // Only re-set the node (and thus restart the tunnel) if it was
            // alive at delete time. Idle tunnels stay idle.
            if t.displayState == .alive,
               let node = t.lastNode, !node.isEmpty {
                try? await client.setTunnelNode(t.name, node: node,
                                                user: t.lastUser ?? NSUserName())
            }
            await reloadTunnelsOnly()
            FriendlyText.haptic()
            notchPresenter.show(
                systemImage: "arrow.uturn.backward",
                title: "Restored",
                description: t.name,
                tint: .green
            )
        } catch {
            connectionError = "Couldn't restore: \(error.localizedDescription)"
        }
    }

    /// Clone an existing tunnel: same node/jump/tags/post-connect, next
    /// free port, name = `<original>-copy[-N]`. Returns the new name
    /// (or nil on failure).
    @discardableResult
    func cloneTunnel(_ t: Tunnel) async -> String? {
        let newName = nextCloneName(for: t.name)
        do {
            let newPort = try await client.suggestPort(base: t.localPort + 1)
            _ = try await client.addTunnel(name: newName, localPort: newPort)
            if !t.tags.isEmpty {
                try? await client.setTunnelTags(newName, tags: t.tags)
            }
            if let cmd = t.postConnectCmd, !cmd.isEmpty {
                try? await client.setTunnelPostConnect(newName, cmd: cmd)
            }
            if let jc = t.jumpCandidates {
                try? await client.setTunnelJumpCandidates(newName, candidates: jc)
            }
            if let node = t.lastNode, !node.isEmpty {
                try? await client.setTunnelNode(newName, node: node,
                                                user: t.lastUser ?? NSUserName())
            }
            await reloadTunnelsOnly()
            FriendlyText.haptic()
            notchPresenter.show(
                systemImage: "doc.on.doc.fill",
                title: "Cloned",
                description: "\(t.name) → \(newName)",
                tint: .blue
            )
            return newName
        } catch {
            connectionError = "Clone failed: \(error.localizedDescription)"
            return nil
        }
    }

    private func nextCloneName(for base: String) -> String {
        let stem = base.hasSuffix("-copy") ? String(base.dropLast(5)) : base
        let names = Set(tunnels.map(\.name))
        var candidate = "\(stem)-copy"
        var i = 2
        while names.contains(candidate) {
            candidate = "\(stem)-copy-\(i)"
            i += 1
        }
        return candidate
    }

    func rotateHost(_ host: SSHHost) async {
        inFlightHosts.insert(host.host)
        defer { inFlightHosts.remove(host.host) }
        do { try await client.rotateHost(host.host) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    func toggleMount(_ host: SSHHost) async {
        inFlightHosts.insert(host.host)
        defer { inFlightHosts.remove(host.host) }
        do { try await client.toggleMount(host.host) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    /// Live TOTP code for a host (6-digit, never the secret). Thin passthrough
    /// to the backend client — the TOTP chip calls this and handles failure
    /// itself (it shows a muted placeholder rather than a global banner), so
    /// we deliberately rethrow instead of swallowing into connectionError.
    func hostTOTP(_ host: String) async throws -> BackendClient.TOTPCode {
        try await client.hostTOTP(host)
    }

    // MARK: - Sheet helpers

    func presentNewTunnel() { activeSheet = .newTunnel }
    func presentNodePicker(for tunnel: Tunnel) { activeSheet = .nodePicker(tunnelName: tunnel.name) }
    func presentCustomNode(for tunnelName: String) { activeSheet = .customNode(tunnelName: tunnelName) }
    func presentConfirmDelete(for tunnel: Tunnel) { activeSheet = .confirmDelete(tunnelName: tunnel.name) }
    func presentAddHost() { activeSheet = .addHost }
    func dismissSheet() { activeSheet = nil }

    /// Create a tunnel. Returns nil on success, or a user-displayable error
    /// message on failure (so the sheet can show it inline rather than
    /// duplicating it as a global banner).
    func createTunnel(name: String, localPort: Int, autoStart: Bool = false) async -> String? {
        inFlightTunnels.insert(name)
        defer { inFlightTunnels.remove(name) }
        do {
            _ = try await client.addTunnel(name: name, localPort: localPort)
            if autoStart {
                try? await client.setTunnelAutostart(name, value: true)
            }
            dismissSheet()
            await reloadAll()
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }

    /// Flip a tunnel's auto-start flag. Persistent across daemon restarts.
    func setTunnelAutostart(_ tunnel: Tunnel, value: Bool) async {
        do { try await client.setTunnelAutostart(tunnel.name, value: value) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    /// Pin (or unpin) the tunnel's jump host. nil = auto pick any ready host;
    /// non-nil = priority-ordered list, daemon takes the first ready entry.
    /// If the tunnel is currently alive the daemon restarts it through the
    /// new candidates so the change takes effect immediately.
    func setJumpCandidates(for tunnel: Tunnel, candidates: [String]?) async {
        inFlightTunnels.insert(tunnel.name)
        defer { inFlightTunnels.remove(tunnel.name) }
        do { try await client.setTunnelJumpCandidates(tunnel.name, candidates: candidates) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    func setPostConnect(for tunnel: Tunnel, cmd: String?) async {
        do { try await client.setTunnelPostConnect(tunnel.name, cmd: cmd) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    func setTags(for tunnel: Tunnel, tags: [String]) async {
        do { try await client.setTunnelTags(tunnel.name, tags: tags) }
        catch { connectionError = error.localizedDescription }
        await reloadTunnelsOnly()
    }

    func setUrlPath(for tunnel: Tunnel, path: String?) async {
        do { try await client.setTunnelUrlPath(tunnel.name, path: path) }
        catch { connectionError = error.localizedDescription }
        await reloadTunnelsOnly()
    }

    /// Rename a tunnel. Returns nil on success or an error message.
    @discardableResult
    func renameTunnel(_ tunnel: Tunnel, to newName: String) async -> String? {
        let new = newName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !new.isEmpty, new != tunnel.name else { return nil }
        inFlightTunnels.insert(tunnel.name)
        defer { inFlightTunnels.remove(tunnel.name) }
        do {
            try await client.renameTunnel(old: tunnel.name, new: new)
            await reloadTunnelsOnly()
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }

    /// Apply imported tunnel definitions. Each one is added via the same
    /// add+configure dance as Undo. Existing names are skipped (renaming
    /// is the user's job — silent overwrite would surprise them).
    /// Returns counts so caller can toast a summary.
    func importTunnels(_ imported: [TunnelExportImport.ExportedTunnel])
        async -> (added: Int, skipped: Int, failed: Int)
    {
        var added = 0, skipped = 0, failed = 0
        let existing = Set(tunnels.map(\.name))
        for t in imported {
            if existing.contains(t.name) { skipped += 1; continue }
            do {
                _ = try await client.addTunnel(name: t.name, localPort: t.local_port)
                if t.auto_start {
                    try? await client.setTunnelAutostart(t.name, value: true)
                }
                if !t.tags.isEmpty {
                    try? await client.setTunnelTags(t.name, tags: t.tags)
                }
                if let cmd = t.post_connect_cmd, !cmd.isEmpty {
                    try? await client.setTunnelPostConnect(t.name, cmd: cmd)
                }
                if let jc = t.jump_candidates {
                    try? await client.setTunnelJumpCandidates(t.name, candidates: jc)
                }
                if let node = t.last_node, !node.isEmpty {
                    try? await client.setTunnelNode(t.name, node: node,
                                                    user: t.last_user ?? NSUserName())
                }
                added += 1
            } catch {
                failed += 1
            }
        }
        await reloadTunnelsOnly()
        notchPresenter.show(
            systemImage: "square.and.arrow.down",
            title: "Imported \(added)",
            description: "\(skipped) skipped, \(failed) failed",
            tint: failed > 0 ? .orange : .green
        )
        return (added, skipped, failed)
    }

    /// Best-effort batch start/stop. Toasts a single summary at the end.
    func batchTunnels(action: String, names: [String]) async {
        do {
            let results = try await client.batchTunnels(action: action, names: names)
            let okCount = results.filter { $0.ok }.count
            notchPresenter.show(
                systemImage: action == "start" ? "play.fill" : "stop.fill",
                title: "\(okCount)/\(results.count) \(action)ed",
                description: names.joined(separator: ", "),
                tint: okCount == results.count ? .green : .orange
            )
        } catch { connectionError = error.localizedDescription }
        await reloadTunnelsOnly()
    }

    /// Nuclear reset — stop everything, rebuild every master. Use sparingly.
    func resetAll() async {
        do {
            let r = try await client.resetAll()
            notchPresenter.show(
                systemImage: "exclamationmark.arrow.circlepath",
                title: "Reset complete",
                description: "\(r.tunnelsStopped) tunnels stopped, \(r.mastersRebuilt) masters rebuilding",
                tint: .orange
            )
        } catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    /// Add a new host via daemon. Returns nil on success, error message on failure.
    @discardableResult
    func addHost(host: String, password: String, otpauthURL: String,
                 autoConnect: Bool) async -> String? {
        do {
            _ = try await client.addHost(host: host, password: password,
                                         otpauthURL: otpauthURL,
                                         autoConnect: autoConnect)
            await reloadAll()
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }

    /// Set a node on a tunnel (also kicks off start via set_node on the
    /// daemon side). Returns nil on success or an error message on failure.
    @discardableResult
    func pickNode(for tunnelName: String, node: String, user: String) async -> String? {
        inFlightTunnels.insert(tunnelName)
        defer { inFlightTunnels.remove(tunnelName) }
        do {
            try await client.setTunnelNode(tunnelName, node: node, user: user)
            dismissSheet()
            await reloadAll()
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }
}
