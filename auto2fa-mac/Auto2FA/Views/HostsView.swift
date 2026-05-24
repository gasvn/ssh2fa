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
                HStack {
                    Button(host.active ? "Stop" : "Start") {
                        Task { await appState.toggleHost(host) }
                    }
                    .controlSize(.small)
                }
            }
            .width(min: 70, ideal: 80, max: 100)
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
