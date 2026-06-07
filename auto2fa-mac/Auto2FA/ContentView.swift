import SwiftUI

/// Root of the main window. Two-pane vertical split: hosts on top, tunnels
/// on the bottom (mirrors the TUI layout). Hosts modal sheets via
/// `appState.activeSheet`.
struct ContentView: View {
    @EnvironmentObject var appState: AppState
    @Environment(\.openWindow) private var openWindow
    @State private var confirmingReset = false
    @State private var showingWelcome = false
    @State private var showingPalette = false

    // Body is broken into smaller pieces because the previous monolithic
    // chain of ~140 lines of modifiers tripped SourceKit's
    // "compiler unable to type-check this expression in reasonable time"
    // warning (real builds were ~3s slower because of it).
    var body: some View {
        mainStack
            .toolbar { toolbarItems }
            .confirmationDialog("Reset everything?",
                                isPresented: $confirmingReset,
                                titleVisibility: .visible) {
                Button("Reset", role: .destructive) { Task { await appState.resetAll() } }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Stops every tunnel and rebuilds every active SSH master. Use this when tunnels are wedged in stale/failed state. Your interactive ssh sessions WILL be dropped.")
            }
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
    }

    private var mainStack: some View {
        VStack(spacing: Spacing.l) {
            HostsView().frame(minHeight: 100)
            TunnelsView().frame(minHeight: 200)
        }
        .padding(Spacing.l)
        .frame(minWidth: 700, minHeight: 400)
        .background(windowBackground)
    }

    /// Subtle layered base so the glass cards float and read. On macOS 26 we
    /// extend the window content beneath the toolbar; otherwise a gentle
    /// gradient over the base material.
    @ViewBuilder
    private var windowBackground: some View {
        let gradient = LinearGradient(
            colors: [
                Brand.accent.opacity(0.06),
                Color.clear,
                Color.black.opacity(0.04)
            ],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
        if #available(macOS 26.0, *) {
            Rectangle()
                .fill(.regularMaterial)
                .overlay(gradient)
                .backgroundExtensionEffect()
                .ignoresSafeArea()
        } else {
            Rectangle()
                .fill(.regularMaterial)
                .overlay(gradient)
                .ignoresSafeArea()
        }
    }

    @ToolbarContentBuilder
    private var toolbarItems: some ToolbarContent {
        ToolbarItemGroup {
            Button { appState.presentAddHost() } label: {
                Label("Add Host", systemImage: "server.rack")
            }
            .help("Register a new SSH host (with 2FA)")
            Button { appState.presentNewTunnel() } label: {
                Label("New Tunnel", systemImage: "plus.circle")
            }
            .help("Create a new tunnel (⌘N)")
            Button { confirmingReset = true } label: {
                Label("Reset", systemImage: "exclamationmark.arrow.circlepath")
                    .foregroundStyle(.red)
            }
            .help("Stop every tunnel + rebuild every SSH master (use when things wedge)")
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
        case .addHost:
            AddHostSheet().environmentObject(appState)
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
