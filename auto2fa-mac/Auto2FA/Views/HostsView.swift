import SwiftUI

struct HostsView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        Table(appState.hosts) {
            TableColumn("Host") { host in
                Text(host.host)
                    .fontDesign(.monospaced)
            }
            .width(min: 100, ideal: 140)

            TableColumn("Status") { host in
                HStack(spacing: 6) {
                    Circle()
                        .fill(color(for: host.displayState))
                        .frame(width: 8, height: 8)
                    Text(displayName(for: host.displayState))
                    if host.poolAlive > 0 {
                        Text("(\(host.poolIndex)/\(host.poolAlive))")
                            .foregroundStyle(.secondary)
                            .font(.caption)
                    }
                }
            }
            .width(min: 140, ideal: 200)

            TableColumn("FS") { host in
                Image(systemName: host.isMounted ? "externaldrive.connected.to.line.below.fill" : "externaldrive")
                    .foregroundStyle(host.isMounted ? .green : .secondary)
            }
            .width(min: 40, ideal: 50, max: 60)

            TableColumn("Last Message") { host in
                Text(host.lastMsg)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            TableColumn("") { host in
                HStack(spacing: 4) {
                    Button {
                        Task { await appState.toggleHost(host) }
                    } label: {
                        Image(systemName: host.active ? "stop.fill" : "play.fill")
                    }
                    .help(host.active ? "Stop / disconnect" : "Start / connect")
                    Button {
                        Task {
                            do { try await appState.client.toggleMount(host.host) }
                            catch { appState.connectionError = error.localizedDescription }
                            await appState.reloadAll()
                        }
                    } label: {
                        Image(systemName: host.isMounted ? "eject.fill" : "externaldrive.badge.plus")
                    }
                    .disabled(!host.isMasterReady && !host.isMounted)
                    .help(host.isMounted ? "Unmount remote filesystem" : "Mount remote filesystem (sshfs)")
                    Button {
                        Task {
                            do { try await appState.client.rotateHost(host.host) }
                            catch { appState.connectionError = error.localizedDescription }
                            await appState.reloadAll()
                        }
                    } label: {
                        Image(systemName: "arrow.triangle.2.circlepath")
                    }
                    .disabled(!host.active)
                    .help("Rotate connection pool slot")
                }
                .buttonStyle(.borderless)
            }
            .width(min: 100, ideal: 110, max: 140)
        }
    }

    private func color(for state: SSHHost.DisplayState) -> Color {
        switch state {
        case .connected: return .green
        case .connecting: return .yellow
        case .failed: return .red
        case .stopped: return .secondary
        case .unknown: return .secondary
        }
    }

    private func displayName(for state: SSHHost.DisplayState) -> String {
        switch state {
        case .connected: return "Connected"
        case .connecting: return "Connecting…"
        case .failed: return "Failed"
        case .stopped: return "Stopped"
        case .unknown: return "Unknown"
        }
    }
}
