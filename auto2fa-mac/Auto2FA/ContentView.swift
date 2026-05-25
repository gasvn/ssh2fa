import SwiftUI

/// Root of the main window. Two-pane vertical split: hosts on top, tunnels
/// on the bottom (mirrors the TUI layout). Hosts modal sheets via
/// `appState.activeSheet`.
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
        .toolbar {
            ToolbarItemGroup {
                Button {
                    appState.presentAddHost()
                } label: {
                    Label("Add Host", systemImage: "server.rack")
                }
                .help("Register a new SSH host (with 2FA)")
                Button {
                    appState.presentNewTunnel()
                } label: {
                    Label("New Tunnel", systemImage: "plus.circle.fill")
                }
                // ⌘N is wired on File → New Tunnel… (Auto2FAApp.commands)
                // — keeping it off the toolbar to avoid duplicate shortcuts.
                .help("Create a new tunnel (⌘N)")
            }
        }
        .overlay(alignment: .top) {
            if let err = appState.connectionError {
                Text(err)
                    .font(.callout.weight(.medium))
                    .padding(.horizontal, 12).padding(.vertical, 6)
                    .background(.red.opacity(0.85), in: Capsule())
                    .foregroundStyle(.white)
                    .padding(.top, 8)
                    .onTapGesture { appState.connectionError = nil }
            }
        }
        // Sheets — bind to a derived value that's nil for .confirmDelete so the
        // sheet machinery doesn't flash an empty sheet alongside the
        // confirmation dialog below.
        .sheet(item: sheetBinding()) { sheet in
            switch sheet {
            case .newTunnel:
                NewTunnelSheet()
                    .environmentObject(appState)
            case .nodePicker(let name):
                NodePickerSheet(tunnelName: name)
                    .environmentObject(appState)
            case .customNode(let name):
                CustomNodeSheet(tunnelName: name)
                    .environmentObject(appState)
            case .addHost:
                AddHostSheet()
                    .environmentObject(appState)
            case .confirmDelete:
                EmptyView()  // unreachable — filtered out in sheetBinding()
            }
        }
        .confirmationDialog(
            confirmDeleteTitle(),
            isPresented: confirmDeleteBinding(),
            titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                if case let .confirmDelete(name) = appState.activeSheet,
                   let t = appState.tunnels.first(where: { $0.name == name }) {
                    Task {
                        await appState.deleteTunnel(t)
                        appState.dismissSheet()
                    }
                }
            }
            Button("Cancel", role: .cancel) { appState.dismissSheet() }
        }
        // bootstrap() is called from Auto2FAApp's WindowGroup .task, AFTER
        // it ensures the daemon is running. Doing it here too would race.
    }

    private func confirmDeleteTitle() -> String {
        if case let .confirmDelete(name) = appState.activeSheet {
            return "Delete tunnel ‘\(name)’?"
        }
        return ""
    }

    private func confirmDeleteBinding() -> Binding<Bool> {
        Binding(
            get: { if case .confirmDelete = appState.activeSheet { return true }; return false },
            set: { newValue in if !newValue { appState.dismissSheet() } }
        )
    }

    /// Sheet binding that filters out `.confirmDelete` — that case is shown
    /// via `.confirmationDialog`, not a real sheet, and SwiftUI would
    /// otherwise flash an empty sheet for it.
    private func sheetBinding() -> Binding<ActiveSheet?> {
        Binding(
            get: {
                switch appState.activeSheet {
                case .confirmDelete, nil: return nil
                case .newTunnel, .nodePicker, .customNode, .addHost: return appState.activeSheet
                }
            },
            set: { newValue in if newValue == nil { appState.dismissSheet() } }
        )
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
