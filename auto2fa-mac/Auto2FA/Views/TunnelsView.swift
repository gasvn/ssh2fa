import SwiftUI

/// Status dot that pulses (scale + opacity) when `animated` is true.
/// Used for the .starting state so users can see the system is still
/// working rather than wondering if it wedged.
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
            VStack(spacing: 12) {
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
        } else {
            VStack(spacing: 0) {
                filterBar
                Divider()
                tunnelsTable
                    .controlSize(compactRows ? .small : .regular)
                    .font(compactRows ? .caption : .body)
            }
        }
    }

    private var filterBar: some View {
        VStack(spacing: 6) {
            HStack(spacing: 8) {
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
                    HStack(spacing: 6) {
                        tagChip("All", isActive: activeTagFilter == nil) {
                            activeTagFilter = nil
                        }
                        ForEach(allTags, id: \.self) { tag in
                            tagChip(tag, isActive: activeTagFilter == tag) {
                                activeTagFilter = (activeTagFilter == tag) ? nil : tag
                            }
                        }
                    }
                    .padding(.horizontal, 8)
                }
            }
        }
        .padding(8)
        .background(.bar)
    }

    @ViewBuilder
    private func tagChip(_ label: String, isActive: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                .font(.caption.weight(.medium))
                .padding(.horizontal, 10).padding(.vertical, 4)
                .background(isActive ? Color.accentColor : Color.gray.opacity(0.15),
                            in: Capsule())
                .foregroundColor(isActive ? .white : .primary)
        }
        .buttonStyle(.plain)
    }

    private var tunnelsTable: some View {
        Table(visibleTunnels, selection: $selection) {
                TableColumn("Name") { t in
                    VStack(alignment: .leading, spacing: 0) {
                        HStack(spacing: 4) {
                            Text(t.name).fontDesign(.monospaced)
                            if t.autoStart {
                                Image(systemName: "bolt.fill")
                                    .font(.caption2)
                                    .foregroundStyle(.yellow)
                                    .help("Starts automatically when the daemon boots")
                            }
                            if t.postConnectCmd != nil {
                                Image(systemName: "terminal.fill")
                                    .font(.caption2)
                                    .foregroundStyle(.blue)
                                    .help("Has a post-connect command")
                            }
                        }
                        if let aliveTxt = t.aliveSince(), t.displayState != .alive {
                            Text(aliveTxt)
                                .font(.caption2)
                                .foregroundStyle(.tertiary)
                        }
                    }
                }
                .width(min: 100, ideal: 130)

                TableColumn("Local → Remote") { t in
                    Text(":\(t.localPort) → :\(t.remotePort)")
                        .fontDesign(.monospaced)
                        .foregroundStyle(.secondary)
                }
                .width(min: 100, ideal: 130)

                TableColumn("Node") { t in
                    if let n = t.lastNode {
                        Text(n).fontDesign(.monospaced).lineLimit(1)
                    } else {
                        Text("(no node yet)").foregroundStyle(.tertiary).italic()
                    }
                }

                TableColumn("Via") { t in
                    // Clickable Menu — left-click opens jump-picker so the
                    // user can pin this tunnel to a specific login node OR
                    // restore Auto. The label shows the CURRENT active
                    // jump (with a small lock if pinned) so at-a-glance
                    // status stays familiar.
                    Menu {
                        jumpPickerMenu(for: t)
                    } label: {
                        HStack(spacing: 4) {
                            if let pinned = t.jumpCandidates, !pinned.isEmpty {
                                Image(systemName: "pin.fill")
                                    .font(.caption2)
                                    .foregroundStyle(.orange)
                            }
                            Text(t.activeJump ?? (t.jumpCandidates?.first ?? "—"))
                                .foregroundStyle(.secondary)
                        }
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    .help(t.jumpCandidates == nil
                          ? "Auto — any ready host. Click to pin."
                          : "Pinned to \(t.jumpCandidates!.joined(separator: ", ")). Click to change.")
                }
                .width(min: 80, ideal: 100)

                TableColumn("Status") { t in
                    HStack(spacing: 6) {
                        if isBusy(t) {
                            ProgressView()
                                .controlSize(.small)
                                .scaleEffect(0.7)
                            Text(busyLabel(t))
                                .fontWeight(.medium)
                                .foregroundStyle(.orange)
                        } else {
                            PulsingDot(color: color(for: t.displayState),
                                       animated: t.displayState == .starting)
                            Text(displayName(for: t.displayState))
                                .fontWeight(.medium)
                        }
                        Text(FriendlyText.tunnelStatusBlurb(t))
                            .foregroundStyle(.secondary)
                            .font(.caption)
                            .lineLimit(1)
                            .help(t.lastMsg)
                    }
                    // Flash yellow briefly whenever the tunnel's status
                    // string changes — helps the eye catch quick
                    // transitions like starting → alive on a busy table.
                    .changeHighlight(t.status)
                }
                .width(min: 200)

                TableColumn("") { t in
                    let busy = isBusy(t)
                    HStack(spacing: 4) {
                        Button {
                            Task { await appState.toggleTunnel(t) }
                        } label: {
                            if busy {
                                ProgressView().controlSize(.small).scaleEffect(0.6)
                                    .frame(width: 14, height: 14)
                            } else {
                                Image(systemName: t.displayState == .alive ? "stop.fill" : "play.fill")
                            }
                        }
                        .help(t.displayState == .alive ? "Stop" : "Start")
                        .disabled(busy)
                        Button {
                            appState.presentNodePicker(for: t)
                        } label: {
                            Image(systemName: "list.bullet.rectangle")
                        }
                        .help("Pick a node from squeue")
                        .disabled(busy)
                        Button {
                            openInBrowser(t)
                        } label: {
                            Image(systemName: "safari")
                        }
                        .help("Open localhost:\(t.localPort) in browser")
                        .disabled(busy || t.displayState != .alive)
                        Button {
                            copyURL(t.url)
                        } label: {
                            Image(systemName: "doc.on.doc")
                        }
                        .help("Copy localhost:\(t.localPort)")
                        Button {
                            detailsForTunnel = t
                        } label: {
                            Image(systemName: "info.circle")
                        }
                        .help("Activity log + post-connect command")
                        Button {
                            appState.presentConfirmDelete(for: t)
                        } label: {
                            Image(systemName: "trash")
                        }
                        .help("Delete tunnel")
                        .disabled(busy)
                    }
                    .buttonStyle(.borderless)
                }
                .width(min: 170, ideal: 190)
            }
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
            // Keyboard handlers — fire when the table has focus and a row
            // is selected. Cuts the rounds-trip-to-mouse loop in half for
            // power users.
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
    /// Multi-select keyboard ops are handled by the batch toolbar buttons
    /// instead of these single-row shortcuts.
    private var singleSelectedTunnel: Tunnel? {
        guard selection.count == 1, let id = selection.first else { return nil }
        return appState.tunnels.first { $0.id == id }
    }

    @ViewBuilder
    private func renameSheet(for t: Tunnel) -> some View {
        VStack(alignment: .leading, spacing: 12) {
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

    @ViewBuilder
    private func tagEditorMenu(for t: Tunnel) -> some View {
        // Quick toggles for existing tags, plus a "New tag" form via prompt.
        let existing = Set(t.tags)
        // Known tags = union across all tunnels (so users can apply
        // already-used ones quickly).
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

    private func copyURL(_ url: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(url, forType: .string)
    }

    /// Menu builder used by both the "Via" column dropdown and the right-
    /// click "Use jump host" submenu. Currently-selected mode (auto vs.
    /// pinned to a specific host) shows a leading checkmark via SF symbol.
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
        // Only list hosts that exist as managers — typos / disabled hosts
        // would just wedge the tunnel.
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

    /// Busy = we just clicked something (inFlightTunnels) OR the daemon is
    /// reporting starting. tunnel_toggle's RPC is slow (awaits the 10s probe)
    /// so inFlightTunnels usually overlaps with .starting, but the pick_node
    /// path can take longer than the RPC and only .starting catches that
    /// tail end.
    private func isBusy(_ t: Tunnel) -> Bool {
        if appState.inFlightTunnels.contains(t.name) { return true }
        return t.displayState == .starting
    }

    private func busyLabel(_ t: Tunnel) -> String {
        let msg = t.lastMsg.trimmingCharacters(in: .whitespacesAndNewlines)
        return msg.isEmpty ? "Working…" : msg
    }

    private func openInBrowser(_ t: Tunnel) {
        // browserURL prepends http:// + appends per-tunnel url_path
        // (e.g. "/?token=abc123" for jupyter), so the user lands on the
        // actual usable page rather than a generic localhost:N.
        guard let url = URL(string: t.browserURL) else { return }
        NSWorkspace.shared.open(url)
        appState.notchPresenter.show(
            systemImage: "safari.fill",
            title: t.name,
            description: "opening \(t.browserURL)",
            tint: .blue
        )
    }

    private func color(for state: Tunnel.DisplayState) -> Color {
        switch state {
        case .alive: return .green
        case .starting: return .yellow
        case .stale: return .red
        case .idle: return .secondary
        case .portBusy: return .red
        case .failed: return .red
        case .unknown: return .secondary
        }
    }

    private func displayName(for state: Tunnel.DisplayState) -> String {
        switch state {
        case .alive: return "Connected"
        case .starting: return "Connecting…"
        case .stale: return "Stale"
        case .idle: return "Idle"
        case .portBusy: return "Port busy"
        case .failed: return "Failed"
        case .unknown: return "—"
        }
    }
}
