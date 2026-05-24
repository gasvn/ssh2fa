import SwiftUI

/// Root of the main window. Two-pane vertical split: hosts on top, tunnels
/// on the bottom (mirrors the TUI layout).
struct ContentView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(spacing: 0) {
            sectionTitle("HOSTS", icon: "server.rack")
            HostsView()
                .frame(minHeight: 100)

            Divider()

            sectionTitle("TUNNELS", icon: "point.3.connected.trianglepath.dotted")
            TunnelsView()
                .frame(minHeight: 200)
        }
        .frame(minWidth: 700, minHeight: 400)
        .background(.regularMaterial)
        .overlay(alignment: .top) {
            if let err = appState.connectionError {
                Text(err)
                    .font(.callout.weight(.medium))
                    .padding(.horizontal, 12).padding(.vertical, 6)
                    .background(.red.opacity(0.85), in: Capsule())
                    .foregroundStyle(.white)
                    .padding(.top, 8)
            }
        }
        .task { await appState.bootstrap() }
    }

    @ViewBuilder
    private func sectionTitle(_ text: String, icon: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
            Text(text).fontWeight(.semibold)
            Spacer()
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12).padding(.vertical, 6)
        .background(.thickMaterial)
    }
}
