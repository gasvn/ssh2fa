import SwiftUI

/// Root of the main window. Two-pane vertical split: hosts on top, tunnels
/// on the bottom (mirrors the TUI layout). Hosts modal sheets via
/// `appState.activeSheet`.
struct ContentView: View {
    @EnvironmentObject var appState: AppState
    @State private var confirmingReset = false
    @State private var showingWelcome = false
    @State private var showingPalette = false

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
                .help("Create a new tunnel (⌘N)")
                Button {
                    confirmingReset = true
                } label: {
                    Label("Reset", systemImage: "exclamationmark.arrow.circlepath")
                        .foregroundStyle(.red)
                }
                .help("Stop every tunnel + rebuild every SSH master (use when things wedge)")
            }
        }
        .confirmationDialog("Reset everything?",
                            isPresented: $confirmingReset,
                            titleVisibility: .visible) {
            Button("Reset", role: .destructive) {
                Task { await appState.resetAll() }
            }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("Stops every tunnel and rebuilds every active SSH master. Use this when tunnels are wedged in stale/failed state. Your interactive ssh sessions WILL be dropped.")
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
        .overlay(alignment: .bottom) {
            // Undo snackbar — visible for ~8s after a tunnel delete.
            if let deleted = appState.undoableDelete {
                HStack(spacing: 12) {
                    Image(systemName: "trash")
                        .foregroundStyle(.secondary)
                    Text("Deleted '\(deleted.name)'")
                        .font(.callout)
                    Spacer(minLength: 12)
                    Button("Undo") {
                        Task { await appState.undoDelete() }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    Button {
                        appState.undoableDelete = nil
                    } label: { Image(systemName: "xmark") }
                        .buttonStyle(.borderless)
                }
                .padding(.horizontal, 14).padding(.vertical, 10)
                .background(.thickMaterial, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10)
                    .stroke(Color.gray.opacity(0.25), lineWidth: 1))
                .padding(.bottom, 16)
                .frame(maxWidth: 360)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(.easeInOut(duration: 0.2), value: appState.undoableDelete?.name)
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
        .sheet(isPresented: $showingWelcome) {
            WelcomeSheet().environmentObject(appState)
        }
        .sheet(isPresented: $showingPalette) {
            CommandPalette().environmentObject(appState)
        }
        .onChange(of: appState.hosts.count) { _, _ in maybeShowWelcome() }
        .onAppear { maybeShowWelcome() }
        .onReceive(NotificationCenter.default.publisher(for: .a2fShowPalette)) { _ in
            showingPalette = true
        }
        // bootstrap() is called from Auto2FAApp's WindowGroup .task, AFTER
        // it ensures the daemon is running. Doing it here too would race.
    }

    /// Show the welcome sheet on first launch where the daemon reports no
    /// hosts AND the user hasn't dismissed it before. Once they hit Skip
    /// or Add Host we set the flag and never re-show.
    private func maybeShowWelcome() {
        let seen = UserDefaults.standard.bool(forKey: SettingsKey.welcomeShown)
        if !seen && appState.hosts.isEmpty {
            showingWelcome = true
        }
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
