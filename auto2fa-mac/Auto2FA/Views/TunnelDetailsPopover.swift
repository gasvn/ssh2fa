import SwiftUI

/// Per-tunnel detail / debug popover. Two sections:
///   - Recent activity (ring buffer from daemon, ~200 entries, polled 2s)
///   - Post-connect command editor (shell snippet that runs each time the
///     tunnel transitions to alive; clear by leaving empty)
///
/// Triggered from a "info.circle" button on each tunnel row.
struct TunnelDetailsPopover: View {
    @EnvironmentObject var appState: AppState
    let tunnel: Tunnel

    @State private var events: [BackendClient.TunnelEvent] = []
    @State private var pollTask: Task<Void, Never>?
    @State private var postConnectDraft: String = ""
    @State private var urlPathDraft: String = ""
    @State private var saveStatus: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(tunnel.name).font(.headline)
                    Text(":\(tunnel.localPort) → \(tunnel.lastNode ?? "—"):\(tunnel.remotePort)")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .padding([.horizontal, .top])
            .padding(.bottom, 8)

            Divider()

            // Stats strip — cheap glance summary above the activity feed.
            HStack(spacing: 16) {
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
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(Color.gray.opacity(0.04))

            Divider()

            sectionHeader("Recent activity")
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    if events.isEmpty {
                        Text("No events yet.")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 8)
                    } else {
                        ForEach(events.reversed()) { e in
                            HStack(alignment: .top, spacing: 8) {
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
                            .padding(.horizontal, 12)
                            .padding(.vertical, 2)
                        }
                    }
                }
            }
            .frame(minHeight: 160, maxHeight: 220)
            .background(Color.gray.opacity(0.05))

            Divider()

            sectionHeader("Browser URL suffix")
            VStack(alignment: .leading, spacing: 6) {
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
                            saveStatus = "Saved"
                        }
                    }
                    .disabled(urlPathDraft == (tunnel.urlPath ?? ""))
                }
                Text("Preview: ").font(.caption).foregroundStyle(.secondary) +
                Text(previewURL()).font(.caption.monospaced())
            }
            .padding(12)

            Divider()

            sectionHeader("Run on connect")
            VStack(alignment: .leading, spacing: 6) {
                Text("Runs each time this tunnel transitions to alive. Env: AUTO2FA_TUNNEL_NAME, AUTO2FA_LOCAL_PORT, AUTO2FA_NODE, AUTO2FA_JUMP, AUTO2FA_URL.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                TextEditor(text: $postConnectDraft)
                    .font(.body.monospaced())
                    .frame(height: 60)
                    .overlay(RoundedRectangle(cornerRadius: 4)
                        .stroke(Color.gray.opacity(0.3), lineWidth: 1))
                HStack {
                    Button("Clear") {
                        postConnectDraft = ""
                        Task {
                            await appState.setPostConnect(for: tunnel, cmd: nil)
                            saveStatus = "Cleared"
                        }
                    }
                    .disabled(postConnectDraft.isEmpty && tunnel.postConnectCmd == nil)
                    Spacer()
                    if let s = saveStatus {
                        Text(s).font(.caption).foregroundStyle(.secondary)
                    }
                    Button("Save") {
                        Task {
                            let trimmed = postConnectDraft.trimmingCharacters(in: .whitespacesAndNewlines)
                            await appState.setPostConnect(for: tunnel, cmd: trimmed.isEmpty ? nil : trimmed)
                            saveStatus = "Saved"
                        }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(postConnectDraft == (tunnel.postConnectCmd ?? ""))
                }
            }
            .padding(12)
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

    @ViewBuilder
    private func statCell(label: String, value: String, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.callout.weight(.medium))
                .foregroundColor(color)
        }
    }

    private func sectionHeader(_ t: String) -> some View {
        Text(t.uppercased())
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 12)
            .padding(.top, 10)
            .padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
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
