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

    var id: String {
        switch self {
        case .newTunnel: return "newTunnel"
        case .nodePicker(let n): return "nodePicker:\(n)"
        case .customNode(let n): return "customNode:\(n)"
        case .confirmDelete(let n): return "confirmDelete:\(n)"
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

    let client = BackendClient()
    private var eventTask: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?

    func bootstrap() async {
        NSLog("[Auto2FA] bootstrap: connecting to daemon")
        do {
            try await client.connect()
            connectionError = nil
            NSLog("[Auto2FA] bootstrap: connected OK")
            // Confirm to the user that the notch is alive — this also serves
            // as a "hello world" so they can see Dynamic Notch working
            // without first having to start a tunnel.
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
        startPollFallback()
    }

    func reloadAll() async {
        do {
            self.hosts = try await client.listHosts()
            self.tunnels = try await client.listTunnels()
        } catch {
            connectionError = error.localizedDescription
        }
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
            await reloadAll()
        case .tunnelChanged(let name, let status, let lastMsg, _):
            await reloadAll()
            maybeShowNotch(name: name, status: status, lastMsg: lastMsg)
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

    private func maybeShowNotch(name: String, status: String, lastMsg: String) {
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
        do { try await client.toggleHost(host.host) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    func toggleTunnel(_ tunnel: Tunnel) async {
        do { try await client.toggleTunnel(tunnel.name) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    func deleteTunnel(_ tunnel: Tunnel) async {
        do { try await client.removeTunnel(tunnel.name) }
        catch { connectionError = error.localizedDescription }
        await reloadAll()
    }

    // MARK: - Sheet helpers

    func presentNewTunnel() { activeSheet = .newTunnel }
    func presentNodePicker(for tunnel: Tunnel) { activeSheet = .nodePicker(tunnelName: tunnel.name) }
    func presentCustomNode(for tunnelName: String) { activeSheet = .customNode(tunnelName: tunnelName) }
    func presentConfirmDelete(for tunnel: Tunnel) { activeSheet = .confirmDelete(tunnelName: tunnel.name) }
    func dismissSheet() { activeSheet = nil }

    /// Create a tunnel. Returns nil on success, or a user-displayable error
    /// message on failure (so the sheet can show it inline rather than
    /// duplicating it as a global banner).
    func createTunnel(name: String, localPort: Int) async -> String? {
        do {
            _ = try await client.addTunnel(name: name, localPort: localPort)
            dismissSheet()
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
