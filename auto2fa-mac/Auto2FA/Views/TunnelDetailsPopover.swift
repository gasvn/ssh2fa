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
        .frame(width: 480)
        .task {
            postConnectDraft = tunnel.postConnectCmd ?? ""
            await refresh()
            startPolling()
        }
        .onDisappear { pollTask?.cancel() }
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
        pollTask?.cancel()
        let name = tunnel.name
        pollTask = Task { [weak appState] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard let state = appState else { return }
                if let fresh = try? await state.client.tunnelEvents(name) {
                    await MainActor.run { self.events = fresh }
                }
            }
        }
    }
}
