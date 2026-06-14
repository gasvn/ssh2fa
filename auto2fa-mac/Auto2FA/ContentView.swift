import SwiftUI

/// Root of the main window. Two-pane vertical split: hosts on top, tunnels
/// on the bottom (mirrors the TUI layout). Hosts modal sheets via
/// `appState.activeSheet`.
struct ContentView: View {
    @EnvironmentObject var appState: AppState
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings
    @State private var showingWelcome = false
    @State private var showingPalette = false
    @FocusState private var searchFocused: Bool

    // Body is broken into smaller pieces because the previous monolithic
    // chain of ~140 lines of modifiers tripped SourceKit's
    // "compiler unable to type-check this expression in reasonable time"
    // warning (real builds were ~3s slower because of it).
    var body: some View {
        mainStack
            .overlay(alignment: .top) { errorBanner }
            .overlay(alignment: .bottom) { undoSnackbar }
            .animation(.easeInOut(duration: 0.2), value: appState.undoableDelete?.name)
            .sheet(item: sheetBinding()) { sheet in sheetContent(for: sheet) }
            .confirmationDialog(
                confirmDeleteTitle(),
                isPresented: confirmDeleteBinding(),
                titleVisibility: .visible
            ) {
                Button("Delete", role: .destructive) { performConfirmedDelete() }
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
            .onReceive(NotificationCenter.default.publisher(for: .a2fShowLogs)) { _ in
                openWindow(id: "logs")
            }
            .onReceive(NotificationCenter.default.publisher(for: .a2fShowSettings)) { _ in
                openSettings()
            }
    }

    private var mainStack: some View {
        VStack(spacing: Spacing.l) {
            HostsView().frame(minHeight: 100)
            TunnelsView().frame(minHeight: 200)
        }
        .padding(Spacing.l)
        .frame(minWidth: 700, minHeight: 400)
        // Window is clear (wallpaper shows through, set in Auto2FAApp); the
        // host/tunnel lists carry their own real Liquid Glass cards.
        .toolbar { mainToolbar }
        // ⌘F focuses the toolbar search field.
        .background {
            Button("") { searchFocused = true }
                .keyboardShortcut("f", modifiers: .command)
                .opacity(0)
                .accessibilityHidden(true)
        }
    }

    @ToolbarContentBuilder
    private var mainToolbar: some ToolbarContent {
        ToolbarItem(placement: .principal) {
            HStack(spacing: Spacing.xs) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search hosts & tunnels", text: $appState.searchQuery)
                    .textFieldStyle(.plain)
                    .frame(minWidth: 180)
                    .focused($searchFocused)
            }
        }
        // Add Host / New Tunnel live in their section headers; Logs / Export /
        // Import live in the menu bar (Window / File). The toolbar gets a DIRECT
        // Settings button — one click opens it, no dropdown.
        ToolbarItem(placement: .primaryAction) {
            Button { openSettings() } label: {
                Label("Settings", systemImage: "gearshape")
            }
            .help("Settings (⌘,)")
        }
    }

    @ViewBuilder
    private var errorBanner: some View {
        if let err = appState.connectionError {
            HStack(spacing: Spacing.s) {
                Image(systemName: "exclamationmark.circle.fill")
                    .foregroundStyle(.red)
                Text(FriendlyText.friendlyError(err))
                    .font(.callout.weight(.medium))
                    .foregroundStyle(.primary)
            }
            .padding(.horizontal, Spacing.m)
            .padding(.vertical, Spacing.s)
            .glassChrome()
            .overlay(
                RoundedRectangle(cornerRadius: Radius.control, style: .continuous)
                    .strokeBorder(Color.red.opacity(0.45), lineWidth: 1)
            )
            .padding(.top, Spacing.m)
            .transition(.move(edge: .top).combined(with: .opacity))
            .onTapGesture { appState.connectionError = nil }
            .help(err)  // hover for the raw underlying message
        }
    }

    @ViewBuilder
    private var undoSnackbar: some View {
        if let deleted = appState.undoableDelete {
            HStack(spacing: Spacing.m) {
                Image(systemName: "trash")
                    .foregroundStyle(.secondary)
                Text("Deleted '\(deleted.name)'")
                    .font(.callout)
                Spacer(minLength: Spacing.m)
                Button("Undo") { Task { await appState.undoDelete() } }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                Button {
                    appState.undoableDelete = nil
                } label: {
                    Image(systemName: "xmark")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.borderless)
            }
            .padding(.horizontal, Spacing.l)
            .padding(.vertical, Spacing.m)
            .glassCard(cornerRadius: Radius.control)
            .padding(.bottom, Spacing.l)
            .frame(maxWidth: 380)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }

    @ViewBuilder
    private func sheetContent(for sheet: ActiveSheet) -> some View {
        switch sheet {
        case .newTunnel:
            NewTunnelSheet().environmentObject(appState)
        case .nodePicker(let name):
            NodePickerSheet(tunnelName: name).environmentObject(appState)
        case .customNode(let name):
            CustomNodeSheet(tunnelName: name).environmentObject(appState)
        case .addHost(let alias):
            AddHostSheet(prefillAlias: alias).environmentObject(appState)
        case .confirmDelete:
            EmptyView()  // unreachable — sheetBinding filters this case to nil
        }
    }

    private func performConfirmedDelete() {
        if case let .confirmDelete(name) = appState.activeSheet,
           let t = appState.tunnels.first(where: { $0.name == name }) {
            Task {
                await appState.deleteTunnel(t)
                appState.dismissSheet()
            }
        }
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
}
