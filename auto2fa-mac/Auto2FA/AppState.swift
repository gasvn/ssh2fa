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
    @Published var activeSheet: ActiveSheet?
    /// Names of hosts/tunnels with an action currently in flight (toggle,
    /// pick_node, delete). UI uses this to swap the action button for a
    /// spinner and overlay a "Working…" status so the user sees that their
    /// click was received — daemon-side operations can take 10-30s while
    /// they probe the local port / wait for SSH to settle.
    @Published var inFlightHosts: Set<String> = []
    @Published var inFlightTunnels: Set<String> = []

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
    /// surface a banner + show a notch toast and kick off a backoff retry.
    /// On reconnect we clear the banner and re-bootstrap state (since the
    /// daemon may have restarted with new state).
    private var connWatcherTask: Task<Void, Never>?
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
                    await self.client.reconnectWithBackoff()
                }
            }
        }
    }

    func reloadAll() async {
        do {
            self.hosts = try await client.listHosts()
            self.tunnels = try await client.listTunnels()
            updateDockBadge()
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
        } catch { connectionError = error.localizedDescription }
    }

    /// Set the Dock-tile badge to the # of alive tunnels (or to the # of
    /// failed things prefixed with "!"). Fires whenever state reloads.
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
                await self?.reloadAll()
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
            // Hand-off: post a macOS notification on hard failures so the
            // user knows even if the app/notch is occluded.
            if status == "failed" || status == "stale" {
                MacNotifications.post(
                    title: "Tunnel \(name) \(status)",
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
    private func maybeAutoOpenBrowser(for t: Tunnel) {
        guard UserDefaults.standard.bool(forKey: "auto2fa.autoOpenBrowser") else { return }
        var raw = t.url
        if !raw.hasPrefix("http://") && !raw.hasPrefix("https://") {
            raw = "http://" + raw
        }
        if let url = URL(string: raw) {
            NSWorkspace.shared.open(url)
        }
    }

    private func maybeShowNotch(name: String, status: String, lastMsg: String) {
        // Settings opt-out — user can mute toasts entirely.
        if UserDefaults.standard.object(forKey: "auto2fa.notch.enabled") != nil,
           UserDefaults.standard.bool(forKey: "auto2fa.notch.enabled") == false {
            return
        }
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
            systemImage: host.displayState == .connected ? "stop.fill" : "arrow.triangle.2.circlepath",
            title: host.displayState == .connected ? "Stopping" : "Starting",
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
            systemImage: tunnel.displayState == .alive ? "stop.fill" : "arrow.triangle.2.circlepath",
            title: tunnel.displayState == .alive ? "Stopping" : "Starting",
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
