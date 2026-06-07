import SwiftUI

/// Two-line, native-minimal row for a single tunnel.
///
/// Line 1: status badge · name (mono) + autostart/post-connect glyphs ·
///         :local→:remote · node · trailing hover actions.
/// Line 2: aliveSince caption · clickable "via <jump>" menu · fail count ·
///         tag capsules.
///
/// All actions route through the shared `AppState` — same SF Symbols, same
/// calls, same disabled logic as the old `Table`-based `TunnelsView`.
/// Presentation only; zero functional change.
struct TunnelRow: View {
    let tunnel: Tunnel
    /// Bindings owned by the parent so context-menu / sheet state stays
    /// centralised (details popover + rename sheet).
    @Binding var detailsForTunnel: Tunnel?
    @Binding var renamingTunnel: Tunnel?
    @Binding var renameDraft: String

    @EnvironmentObject var appState: AppState
    @State private var hovering = false

    // MARK: - Busy logic (verbatim from old TunnelsView)

    /// Busy = we just clicked something (inFlightTunnels) OR the daemon is
    /// reporting starting.
    private var isBusy: Bool {
        if appState.inFlightTunnels.contains(tunnel.name) { return true }
        return tunnel.displayState == .starting
    }

    private var busyLabel: String {
        let msg = tunnel.lastMsg.trimmingCharacters(in: .whitespacesAndNewlines)
        return msg.isEmpty ? "Working…" : msg
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            line1
            line2
        }
        .padding(.vertical, Spacing.s)
        .contentShape(Rectangle())
        // Flash yellow briefly whenever the tunnel's status string changes —
        // helps the eye catch quick transitions like starting → alive.
        .changeHighlight(tunnel.status)
        .hoverLift(hovering)
        .onHover { hovering = $0 }
    }

    // MARK: - Line 1

    private var line1: some View {
        HStack(spacing: Spacing.s) {
            // Status: spinner + progress text while busy, else badge.
            if isBusy {
                HStack(spacing: Spacing.xs) {
                    ProgressView()
                        .controlSize(.small)
                        .scaleEffect(0.7)
                        .frame(width: RowMetric.iconSize, height: RowMetric.iconSize)
                    Text(busyLabel)
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .help(tunnel.lastMsg)
                .layoutPriority(1)
            } else {
                StatusBadge(tunnel: tunnel.displayState,
                            text: FriendlyText.tunnelStatusBlurb(tunnel))
                    .help(tunnel.lastMsg)
                    .layoutPriority(1)
            }

            // Name (mono, primary) + autostart / post-connect glyphs.
            HStack(spacing: Spacing.xs) {
                Text(tunnel.name)
                    .font(.rowTitle)
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if tunnel.autoStart {
                    Image(systemName: "bolt.fill")
                        .font(.caption2)
                        .foregroundStyle(.yellow)
                        .help("Starts automatically when the daemon boots")
                }
                if tunnel.postConnectCmd != nil {
                    Image(systemName: "terminal.fill")
                        .font(.caption2)
                        .foregroundStyle(.blue)
                        .help("Has a post-connect command")
                }
            }

            // :local → :remote (secondary mono).
            Text(":\(tunnel.localPort) → :\(tunnel.remotePort)")
                .font(RowMetric.mono)
                .foregroundStyle(.secondary)
                .lineLimit(1)

            // Node (secondary; "(no node yet)" tertiary italic).
            if let n = tunnel.lastNode {
                Text(n)
                    .font(RowMetric.mono)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            } else {
                Text("(no node yet)")
                    .foregroundStyle(.tertiary)
                    .italic()
                    .lineLimit(1)
            }

            Spacer(minLength: Spacing.s)

            // Hover actions.
            if hovering {
                actions
                    .transition(.opacity)
            }
        }
    }

    // MARK: - Actions (same calls / SF Symbols / disabled logic as old view)

    private var actions: some View {
        HStack(spacing: Spacing.xs) {
            // Start / stop (toggle).
            Button {
                Task { await appState.toggleTunnel(tunnel) }
            } label: {
                if isBusy {
                    ProgressView().controlSize(.small).scaleEffect(0.6)
                        .frame(width: 14, height: 14)
                } else {
                    Image(systemName: tunnel.displayState == .alive ? "stop.fill" : "play.fill")
                }
            }
            .help(tunnel.displayState == .alive ? "Stop" : "Start")
            .disabled(isBusy)

            // Pick node.
            Button {
                appState.presentNodePicker(for: tunnel)
            } label: {
                Image(systemName: "list.bullet.rectangle")
            }
            .help("Pick a node from squeue")
            .disabled(isBusy)

            // Open in browser (disabled if not alive).
            Button {
                openInBrowser(tunnel)
            } label: {
                Image(systemName: "safari")
            }
            .help("Open localhost:\(tunnel.localPort) in browser")
            .disabled(isBusy || tunnel.displayState != .alive)

            // Copy URL.
            Button {
                copyURL(tunnel.url)
            } label: {
                Image(systemName: "doc.on.doc")
            }
            .help("Copy localhost:\(tunnel.localPort)")

            // Details.
            Button {
                detailsForTunnel = tunnel
            } label: {
                Image(systemName: "info.circle")
            }
            .help("Activity log + post-connect command")

            // Delete.
            Button {
                appState.presentConfirmDelete(for: tunnel)
            } label: {
                Image(systemName: "trash")
            }
            .help("Delete tunnel")
            .disabled(isBusy)
        }
        .buttonStyle(.borderless)
    }

    // MARK: - Line 2 (secondary metadata caption)

    private var line2: some View {
        HStack(spacing: Spacing.s) {
            // aliveSince text.
            if let aliveTxt = tunnel.aliveSince() {
                Text(aliveTxt)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            // via <jump> — same clickable jump-host Menu as before.
            Menu {
                jumpPickerMenu(for: tunnel)
            } label: {
                HStack(spacing: Spacing.xs) {
                    if let pinned = tunnel.jumpCandidates, !pinned.isEmpty {
                        Image(systemName: "pin.fill")
                            .font(.caption2)
                            .foregroundStyle(.orange)
                    }
                    Text("via \(tunnel.activeJump ?? (tunnel.jumpCandidates?.first ?? "Auto"))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .help(tunnel.jumpCandidates == nil
                  ? "Auto — any ready host. Click to pin."
                  : "Pinned to \(tunnel.jumpCandidates!.joined(separator: ", ")). Click to change.")

            // Fail count — tinted capsule, red/orange only when > 0.
            if tunnel.failCount > 0 {
                let failTint: Color = tunnel.failCount >= 3 ? .red : .orange
                Text("\(tunnel.failCount) fails")
                    .font(.countBadge)
                    .foregroundStyle(failTint)
                    .lineLimit(1)
                    .padding(.horizontal, Spacing.xs + 2)
                    .padding(.vertical, 1)
                    .background(failTint.opacity(0.15), in: Capsule())
            }

            // Tags as small capsules.
            if !tunnel.tags.isEmpty {
                ForEach(tunnel.tags, id: \.self) { tag in
                    Text(tag)
                        .font(.caption2.weight(.medium))
                        .padding(.horizontal, Spacing.xs + 2)
                        .padding(.vertical, 1)
                        .background(Color.gray.opacity(0.15), in: Capsule())
                        .foregroundStyle(.secondary)
                }
            }

            Spacer(minLength: 0)
        }
        .padding(.leading, RowMetric.iconSize + Spacing.xs)
    }

    // MARK: - Jump-host picker (verbatim from old TunnelsView)

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

    // MARK: - Helpers (verbatim from old TunnelsView)

    private func copyURL(_ url: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(url, forType: .string)
    }

    private func openInBrowser(_ t: Tunnel) {
        // browserURL prepends http:// + appends per-tunnel url_path so the
        // user lands on the actual usable page.
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
