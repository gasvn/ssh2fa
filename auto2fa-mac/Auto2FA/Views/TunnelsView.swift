import SwiftUI

/// Status dot that pulses (scale + opacity) when `animated` is true.
/// Kept for compatibility; the tunnels list now routes status through the
/// shared `StatusDot`/`StatusBadge` components instead.
struct PulsingDot: View {
    let color: Color
    let animated: Bool
    @State private var phase: Bool = false
    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 8, height: 8)
            .scaleEffect(animated && phase ? 1.4 : 1.0)
            .opacity(animated && phase ? 0.5 : 1.0)
            .animation(
                animated ? .easeInOut(duration: 0.8).repeatForever(autoreverses: true) : .default,
                value: phase
            )
            .onAppear { if animated { phase = true } }
            .onChange(of: animated) { _, on in phase = on }
    }
}

struct TunnelsView: View {
    @EnvironmentObject var appState: AppState
    @AppStorage(SettingsKey.compactRows) private var compactRows = false
    @State private var selection: Set<Tunnel.ID> = []
    @State private var detailsForTunnel: Tunnel?
    @State private var filter: String = ""
    @State private var activeTagFilter: String? = nil
    @State private var renamingTunnel: Tunnel? = nil
    @State private var renameDraft: String = ""

    /// All distinct tags currently in use, sorted, for the filter chips.
    private var allTags: [String] {
        Array(Set(appState.tunnels.flatMap { $0.tags })).sorted()
    }

    /// Tunnels passing both the text filter and the tag filter.
    private var visibleTunnels: [Tunnel] {
        let q = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return appState.tunnels.filter { t in
            if let tag = activeTagFilter, !t.tags.contains(tag) { return false }
            if q.isEmpty { return true }
            if t.name.lowercased().contains(q) { return true }
            if (t.lastNode ?? "").lowercased().contains(q) { return true }
            if (t.activeJump ?? "").lowercased().contains(q) { return true }
            if t.tags.contains(where: { $0.lowercased().contains(q) }) { return true }
            return false
        }
    }

    var body: some View {
        if appState.tunnels.isEmpty {
            emptyState
        } else {
            VStack(spacing: 0) {
                filterBar
                Divider()
                tunnelsList
                    .controlSize(compactRows ? .small : .regular)
                    .font(compactRows ? .caption : .body)
            }
        }
    }

    // MARK: - Empty state

    private var emptyState: some View {
        VStack(spacing: Spacing.m) {
            Image(systemName: "sparkles")
                .font(.largeTitle)
                .foregroundStyle(.yellow)
            Text("No tunnels yet")
                .font(.title3)
            Text("Click  +  in the toolbar (or press ⌘N) to create one.")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button {
                appState.presentNewTunnel()
            } label: {
                Label("New tunnel", systemImage: "plus")
            }
            .controlSize(.large)
            .keyboardShortcut(.defaultAction)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }

    // MARK: - Filter bar

    private var filterBar: some View {
        VStack(spacing: Spacing.s) {
            HStack(spacing: Spacing.s) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("filter by name, node, jump, tag…", text: $filter)
                    .textFieldStyle(.roundedBorder)
                if !filter.isEmpty {
                    Button {
                        filter = ""
                    } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.borderless)
                }
                if !selection.isEmpty {
                    Divider().frame(height: 16)
                    Text("\(selection.count) selected")
                        .font(.caption).foregroundStyle(.secondary)
                    Button {
                        Task {
                            await appState.batchTunnels(action: "start",
                                names: Array(selection))
                        }
                    } label: { Label("Start", systemImage: "play.fill") }
                        .controlSize(.small)
                    Button {
                        Task {
                            await appState.batchTunnels(action: "stop",
                                names: Array(selection))
                        }
                    } label: { Label("Stop", systemImage: "stop.fill") }
                        .controlSize(.small)
                }
            }
            if !allTags.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: Spacing.xs + 2) {
                        tagChip("All", isActive: activeTagFilter == nil) {
                            activeTagFilter = nil
                        }
                        ForEach(allTags, id: \.self) { tag in
                            tagChip(tag, isActive: activeTagFilter == tag) {
                                activeTagFilter = (activeTagFilter == tag) ? nil : tag
                            }
                        }
                    }
                    .padding(.horizontal, Spacing.s)
                }
            }
        }
        .padding(Spacing.s)
        .background(.bar)
    }

    @ViewBuilder
    private func tagChip(_ label: String, isActive: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                .font(.caption.weight(.medium))
                .padding(.horizontal, Spacing.s + 2).padding(.vertical, Spacing.xs)
                .background(isActive ? Color.accentColor : Color.gray.opacity(0.15),
                            in: Capsule())
                .foregroundColor(isActive ? .white : .primary)
        }
        .buttonStyle(.plain)
    }

    // MARK: - List

    private var tunnelsList: some View {
        List(selection: $selection) {
            ForEach(visibleTunnels) { t in
                TunnelRow(tunnel: t,
                          detailsForTunnel: $detailsForTunnel,
                          renamingTunnel: $renamingTunnel,
                          renameDraft: $renameDraft)
                    .tag(t.id)
                    .listRowInsets(EdgeInsets(top: 0,
                                              leading: Spacing.m,
                                              bottom: 0,
                                              trailing: Spacing.m))
            }
        }
        .listStyle(.inset)
        .environmentObject(appState)
        .sheet(item: $detailsForTunnel) { t in
            TunnelDetailsPopover(tunnel: t)
                .environmentObject(appState)
        }
        .onReceive(NotificationCenter.default.publisher(for: .a2fShowTunnelDetails)) { note in
            if let name = note.userInfo?["name"] as? String,
               let t = appState.tunnels.first(where: { $0.name == name }) {
                detailsForTunnel = t
            }
        }
        .sheet(item: $renamingTunnel) { t in
            renameSheet(for: t)
        }
        .contextMenu(forSelectionType: Tunnel.ID.self) { ids in
            if let id = ids.first,
               let t = appState.tunnels.first(where: { $0.id == id }) {
                Button(t.displayState == .alive ? "Stop" : "Start") {
                    Task { await appState.toggleTunnel(t) }
                }
                Button("Pick node…") {
                    appState.presentNodePicker(for: t)
                }
                Menu("Use jump host") {
                    jumpPickerMenu(for: t)
                }
                Button("Open in browser") {
                    openInBrowser(t)
                }
                .disabled(t.displayState != .alive)
                Button("Copy localhost:\(t.localPort)") {
                    copyURL(t.url)
                }
                Divider()
                Button("Clone…") {
                    Task { await appState.cloneTunnel(t) }
                }
                Button("Rename…") {
                    renameDraft = t.name
                    renamingTunnel = t
                }
                Menu("Tags") {
                    tagEditorMenu(for: t)
                }
                Toggle("Start on daemon launch", isOn: Binding(
                    get: { t.autoStart },
                    set: { newValue in
                        Task { await appState.setTunnelAutostart(t, value: newValue) }
                    }
                ))
                Divider()
                Button("Delete tunnel", role: .destructive) {
                    appState.presentConfirmDelete(for: t)
                }
            }
        }
        // Keyboard handlers — fire when the list has focus and a row is
        // selected. Cuts the round-trip-to-mouse loop for power users.
        .onKeyPress(.space) {
            guard let t = singleSelectedTunnel else { return .ignored }
            Task { await appState.toggleTunnel(t) }
            return .handled
        }
        .onKeyPress(.return) {
            guard let t = singleSelectedTunnel else { return .ignored }
            appState.presentNodePicker(for: t)
            return .handled
        }
        .onKeyPress(.delete) {
            guard let t = singleSelectedTunnel else { return .ignored }
            appState.presentConfirmDelete(for: t)
            return .handled
        }
        .onKeyPress(keys: ["c"]) { press in
            guard press.modifiers.contains(.command),
                  let t = singleSelectedTunnel else { return .ignored }
            copyURL(t.url)
            FriendlyText.haptic()
            appState.notchPresenter.show(
                systemImage: "doc.on.doc",
                title: "Copied",
                description: t.url,
                tint: .blue
            )
            return .handled
        }
        .onKeyPress(keys: ["o"]) { press in
            guard press.modifiers.contains(.command),
                  let t = singleSelectedTunnel,
                  t.displayState == .alive else { return .ignored }
            openInBrowser(t)
            return .handled
        }
        .onKeyPress(keys: ["d"]) { press in
            guard press.modifiers.contains(.command),
                  let t = singleSelectedTunnel else { return .ignored }
            Task { await appState.cloneTunnel(t) }
            return .handled
        }
    }

    /// Convenience: returns the Tunnel iff EXACTLY one row is selected.
    /// Multi-select keyboard ops are handled by the batch toolbar buttons.
    private var singleSelectedTunnel: Tunnel? {
        guard selection.count == 1, let id = selection.first else { return nil }
        return appState.tunnels.first { $0.id == id }
    }

    // MARK: - Rename sheet

    @ViewBuilder
    private func renameSheet(for t: Tunnel) -> some View {
        VStack(alignment: .leading, spacing: Spacing.m) {
            Text("Rename tunnel").font(.headline)
            Text("Currently: \(t.name)").font(.caption).foregroundStyle(.secondary)
            TextField("new-name", text: $renameDraft)
                .textFieldStyle(.roundedBorder)
            HStack {
                Spacer()
                Button("Cancel") { renamingTunnel = nil }
                    .keyboardShortcut(.cancelAction)
                Button("Rename") {
                    let target = t
                    let newName = renameDraft
                    Task {
                        _ = await appState.renameTunnel(target, to: newName)
                        renamingTunnel = nil
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(renameDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
                          renameDraft == t.name)
            }
        }
        .padding(20)
        .frame(width: 340)
    }

    // MARK: - Tag editor menu

    @ViewBuilder
    private func tagEditorMenu(for t: Tunnel) -> some View {
        // Quick toggles for existing tags, plus a "Clear all".
        let existing = Set(t.tags)
        // Known tags = union across all tunnels.
        let known = Array(Set(appState.tunnels.flatMap { $0.tags })).sorted()
        if known.isEmpty {
            Text("No tags yet — add one from CLI or tunnels.json.")
        } else {
            ForEach(known, id: \.self) { tag in
                Button {
                    var next = Array(existing)
                    if existing.contains(tag) {
                        next.removeAll { $0 == tag }
                    } else {
                        next.append(tag)
                    }
                    Task { await appState.setTags(for: t, tags: next) }
                } label: {
                    Label(tag, systemImage: existing.contains(tag) ? "checkmark" : "circle")
                }
            }
        }
        Divider()
        Button("Clear all tags") {
            Task { await appState.setTags(for: t, tags: []) }
        }
        .disabled(t.tags.isEmpty)
    }

    // MARK: - Jump-host picker (shared with the row's "via" menu)

    @ViewBuilder
    private func jumpPickerMenu(for t: Tunnel) -> some View {
        let isAuto = (t.jumpCandidates == nil) || (t.jumpCandidates?.isEmpty ?? true)
        Button {
            Task { await appState.setJumpCandidates(for: t, candidates: nil) }
        } label: {
            Label("Auto (any ready host)",
                  systemImage: isAuto ? "checkmark" : "circle")
        }
        Divider()
        ForEach(appState.hosts, id: \.host) { host in
            let pinned = (t.jumpCandidates == [host.host])
            Button {
                Task { await appState.setJumpCandidates(for: t, candidates: [host.host]) }
            } label: {
                Label(host.host,
                      systemImage: pinned ? "checkmark"
                                          : (host.isMasterReady ? "circle.fill" : "circle"))
            }
        }
    }

    // MARK: - Helpers

    private func copyURL(_ url: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(url, forType: .string)
    }

    private func openInBrowser(_ t: Tunnel) {
        guard let url = URL(string: t.browserURL) else { return }
        NSWorkspace.shared.open(url)
        appState.notchPresenter.show(
            systemImage: "safari.fill",
            title: t.name,
            description: "opening \(t.browserURL)",
            tint: .blue
        )
    }
}
