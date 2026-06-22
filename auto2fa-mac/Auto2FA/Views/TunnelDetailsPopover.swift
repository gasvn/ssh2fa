import SwiftUI

/// Per-tunnel detail / debug popover. Two sections:
///   - Recent activity (ring buffer from daemon, ~200 entries, polled 2s)
///   - Post-connect command editor (shell snippet that runs each time the
///     tunnel transitions to alive; clear by leaving empty)
///
/// Triggered from a "info.circle" button on each tunnel row.
struct TunnelDetailsPopover: View {
    @EnvironmentObject var appState: AppState
    @Environment(\.dismiss) private var dismiss
    /// Snapshot captured when the popover opened — fallback only.
    let initialTunnel: Tunnel

    /// LIVE tunnel: re-resolved from appState on every render so the status
    /// pill / stats / ports track reality while the sheet is open. The old
    /// `let tunnel` value-copy froze the whole popover at open time (a tunnel
    /// going alive→failed still showed the stale state).
    var tunnel: Tunnel {
        appState.tunnels.first(where: { $0.name == initialTunnel.name }) ?? initialTunnel
    }

    init(tunnel: Tunnel) {
        self.initialTunnel = tunnel
    }

    @State private var events: [BackendClient.TunnelEvent] = []
    @State private var pollTask: Task<Void, Never>?
    @State private var postConnectDraft: String = ""
    @State private var urlPathDraft: String = ""
    // Per-section transient "Saved"/"Cleared" confirmation (auto-clears after a
    // beat). Separate vars so a URL-path save never flashes its status next to
    // the run-on-connect button (they're different sections).
    @State private var urlSaveStatus: String?
    @State private var cmdSaveStatus: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header
            HStack(spacing: Spacing.s) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(tunnel.name).font(.dashTitle)
                    Text(tunnel.isDirect
                         ? ":\(tunnel.localPort) → \(tunnel.directHost ?? "host"):\(tunnel.remotePort) (direct)"
                         : ":\(tunnel.localPort) → \(tunnel.lastNode ?? "—"):\(tunnel.remotePort)")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
                Spacer()
                // Status pill
                StatusBadge(tunnel: tunnel.displayState,
                            text: FriendlyText.tunnelStatusBlurb(tunnel))
            }
            .padding(.horizontal, Spacing.l)
            .padding(.top, Spacing.l)
            .padding(.bottom, Spacing.s)

            Divider()

            // Stats strip — tinted capsule cells for a layered look
            HStack(spacing: Spacing.s) {
                statCell(label: "Connects",
                         value: "\(tunnel.connectCount)",
                         color: .blue)
                statCell(label: "Failures",
                         value: "\(tunnel.failCount)",
                         color: tunnel.failCount > 0 ? .red : .secondary)
                statCell(label: "Uptime",
                         value: tunnel.uptimeHuman,
                         color: .green)
                statCell(label: tunnel.displayState == .alive ? "Alive since" : "Last alive",
                         value: tunnel.aliveSince() ?? "never",
                         color: .primary)
                Spacer()
            }
            .padding(.horizontal, Spacing.l)
            .padding(.vertical, Spacing.s)

            Divider()

            sectionHeader("Recent activity")
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    if events.isEmpty {
                        Text("No events yet.")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, Spacing.m)
                            .padding(.vertical, Spacing.s)
                    } else {
                        ForEach(events.reversed()) { e in
                            HStack(alignment: .top, spacing: Spacing.s) {
                                Text(e.date, format: .dateTime.hour().minute().second())
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(.secondary)
                                    .frame(width: 56, alignment: .leading)
                                Text(e.msg)
                                    .font(.caption.monospaced())
                                    .textSelection(.enabled)
                                    .lineLimit(nil)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .padding(.horizontal, Spacing.m)
                            .padding(.vertical, 2)
                        }
                    }
                }
            }
            .frame(minHeight: 160, maxHeight: 220)
            .groupedContent(cornerRadius: Radius.control)
            .padding(.horizontal, Spacing.l)
            .padding(.vertical, Spacing.xs)

            Divider()

            sectionHeader("Browser URL suffix")
            VStack(alignment: .leading, spacing: Spacing.s) {
                Text("Appended after `http://localhost:\(tunnel.localPort)` when you click 🧭 or auto-open fires. Use this for jupyter `?token=…`, tensorboard `/#scalars`, etc.")
                    .font(.caption).foregroundStyle(.secondary)
                HStack {
                    TextField("/?token=abc123", text: $urlPathDraft)
                        .textFieldStyle(.roundedBorder)
                        .font(.body.monospaced())
                    Button("Save") {
                        Task {
                            let trimmed = urlPathDraft.trimmingCharacters(in: .whitespacesAndNewlines)
                            await appState.setUrlPath(for: tunnel,
                                path: trimmed.isEmpty ? nil : trimmed)
                            urlSaveStatus = "Saved"
                            try? await Task.sleep(nanoseconds: 1_500_000_000)
                            urlSaveStatus = nil
                        }
                    }
                    .disabled(urlPathDraft == (tunnel.urlPath ?? ""))
                    if let s = urlSaveStatus {
                        Text(s).font(.countBadge).foregroundStyle(.secondary)
                    }
                }
                Text("Preview: ").font(.caption).foregroundStyle(.secondary) +
                Text(previewURL()).font(.caption.monospaced())
            }
            .padding(.horizontal, Spacing.l)
            .padding(.bottom, Spacing.s)

            Divider()

            sectionHeader("Run on connect")
            VStack(alignment: .leading, spacing: Spacing.s) {
                Text("Runs each time this tunnel transitions to alive. Env: AUTO2FA_TUNNEL_NAME, AUTO2FA_LOCAL_PORT, AUTO2FA_NODE, AUTO2FA_JUMP, AUTO2FA_URL.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                TextEditor(text: $postConnectDraft)
                    .font(.body.monospaced())
                    .frame(height: 60)
                    .overlay(RoundedRectangle(cornerRadius: Radius.control)
                        .stroke(Color.gray.opacity(0.3), lineWidth: 1))
                HStack {
                    Button("Clear") {
                        postConnectDraft = ""
                        Task {
                            await appState.setPostConnect(for: tunnel, cmd: nil)
                            cmdSaveStatus = "Cleared"
                            try? await Task.sleep(nanoseconds: 1_500_000_000)
                            cmdSaveStatus = nil
                        }
                    }
                    .disabled(postConnectDraft.isEmpty && tunnel.postConnectCmd == nil)
                    Spacer()
                    if let s = cmdSaveStatus {
                        Text(s).font(.countBadge).foregroundStyle(.secondary)
                    }
                    Button("Save") {
                        Task {
                            let trimmed = postConnectDraft.trimmingCharacters(in: .whitespacesAndNewlines)
                            await appState.setPostConnect(for: tunnel, cmd: trimmed.isEmpty ? nil : trimmed)
                            cmdSaveStatus = "Saved"
                            try? await Task.sleep(nanoseconds: 1_500_000_000)
                            cmdSaveStatus = nil
                        }
                    }
                    .disabled(postConnectDraft == (tunnel.postConnectCmd ?? ""))
                }
            }
            .padding(.horizontal, Spacing.l)
            .padding(.bottom, Spacing.l)

            Divider()
            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.cancelAction)   // Esc / explicit close
            }
            .padding(.horizontal, Spacing.l)
            .padding(.vertical, Spacing.s)
        }
        .frame(width: 720)
        .task {
            postConnectDraft = tunnel.postConnectCmd ?? ""
            urlPathDraft = tunnel.urlPath ?? ""
            await refresh()
            startPolling()
        }
        .onDisappear { pollTask?.cancel() }
    }

    private func previewURL() -> String {
        let trimmed = urlPathDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        let suffix = trimmed.isEmpty ? "" :
            (trimmed.hasPrefix("/") || trimmed.hasPrefix("?") ? trimmed : "/" + trimmed)
        return "http://localhost:\(tunnel.localPort)\(suffix)"
    }

    /// Tinted capsule stat cell matching the dashboard's count-pill pattern.
    @ViewBuilder
    private func statCell(label: String, value: String, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.countBadge)
                .foregroundColor(color)
        }
        .padding(.horizontal, Spacing.s)
        .padding(.vertical, Spacing.xs)
        .background(color.opacity(0.10), in: RoundedRectangle(cornerRadius: Radius.control, style: .continuous))
    }

    private func sectionHeader(_ t: String) -> some View {
        Text(t.uppercased())
            .sectionHeaderStyle()
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, Spacing.xs)
    }

    private func refresh() async {
        do {
            events = try await appState.client.tunnelEvents(tunnel.name)
        } catch {
            // Soft fail — events are non-critical
        }
    }

    private func startPolling() {
        // Always cancel any prior task — .task can fire again when SwiftUI
        // re-creates the popover (e.g. user closes and reopens it without
        // a full re-render cycle). Without this, multiple poll loops can
        // overlap, each issuing tunnelEvents IPCs every 2s and stomping
        // each other's `events` writes.
        pollTask?.cancel()
        let name = tunnel.name
        pollTask = Task { [weak appState] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                if Task.isCancelled { return }
                guard let state = appState else { return }
                if let fresh = try? await state.client.tunnelEvents(name) {
                    await MainActor.run { self.events = fresh }
                }
            }
        }
    }
}
