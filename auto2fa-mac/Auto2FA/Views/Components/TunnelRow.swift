import SwiftUI

/// Single-line, dense row for a single tunnel — aligned columns like a clean
/// compact table.
///
/// `[dot] name [⚡][🖥]  :local→:remote  node  via  metadata  <Spacer> [hover actions]`
///
/// Status (Connected/Idle/etc.) is conveyed by the dot colour + the metadata
/// (aliveSince), so no wide status pill is needed. All actions route through
/// the shared `AppState` — same SF Symbols, same calls, same disabled logic as
/// before. Presentation only; zero functional change.
struct TunnelRow: View {
    let tunnel: Tunnel
    /// Bindings owned by the parent so context-menu / sheet state stays
    /// centralised (details popover + rename sheet).
    @Binding var detailsForTunnel: Tunnel?
    @Binding var renamingTunnel: Tunnel?
    @Binding var renameDraft: String

    @EnvironmentObject var appState: AppState
    @AppStorage(SettingsKey.compactRows) private var compactRows = false
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

    /// Tooltip for the whole row — the friendly status blurb + raw last message.
    private var rowTooltip: String {
        let blurb = FriendlyText.tunnelStatusBlurb(tunnel)
        let msg = tunnel.lastMsg.trimmingCharacters(in: .whitespacesAndNewlines)
        if msg.isEmpty || msg == blurb { return blurb }
        return "\(blurb)\n\(msg)"
    }

    var body: some View {
        HStack(spacing: Spacing.s) {
            // Leading status dot (compact — not the wide pill). Pulses while
            // the tunnel is starting.
            StatusDot(tunnel: tunnel.displayState)
                .frame(width: RowMetric.iconSize, height: RowMetric.iconSize)

            // Name (rounded title) + inline autostart / post-connect glyphs.
            // Fixed-ish leading column so the following columns align.
            HStack(spacing: Spacing.xs) {
                Text(tunnel.name)
                    .font(.rowTitle)
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                    .truncationMode(.tail)
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
            .frame(minWidth: 90, alignment: .leading)

            // Ports :local → :remote — fixed column (mono).
            Text(":\(tunnel.localPort) → :\(tunnel.remotePort)")
                .font(.rowIdentifier)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .frame(width: 110, alignment: .leading)

            // Node (secondary; "(no node)" tertiary) — flexible column.
            Group {
                if let n = tunnel.lastNode {
                    Text(n)
                        .font(.rowIdentifier)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                } else {
                    Text("(no node)")
                        .font(.rowMeta)
                        .foregroundStyle(.tertiary)
                        .italic()
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            // via <jump> — the existing clickable jump-host Menu, compact.
            // Hidden on hover (it lives in the `⋯` overflow as "Use jump host").
            if !hovering {
                viaMenu
                    .frame(width: 70, alignment: .leading)

                // Metadata: aliveSince + fail count — compact fixed column.
                metadata
                    .frame(width: 92, alignment: .leading)
            }

            Spacer(minLength: Spacing.s)

            // TRAILING ZONE: at rest the via-menu + metadata above are shown; on
            // hover a right-aligned icon+TEXT action bar (primary actions) + a
            // labeled `⋯` overflow menu replaces it. Row height stays fixed.
            if hovering {
                actions
                    .transition(.opacity)
                overflowMenu
                    .transition(.opacity)
            }
        }
        .animation(.easeInOut(duration: 0.12), value: hovering)
        .padding(.vertical, compactRows ? 1 : 2)
        .frame(minHeight: compactRows ? 22 : RowMetric.minHeight)
        .contentShape(Rectangle())
        // Flash yellow briefly whenever the tunnel's status string changes —
        // helps the eye catch quick transitions like starting → alive.
        .changeHighlight(tunnel.status)
        .help(rowTooltip)
        .hoverLift(hovering)
        .onHover { hovering = $0 }
    }

    // MARK: - via jump-host menu (compact; verbatim behaviour)

    private var viaMenu: some View {
        Menu {
            jumpPickerMenu(for: tunnel)
        } label: {
            HStack(spacing: 2) {
                if let pinned = tunnel.jumpCandidates, !pinned.isEmpty {
                    Image(systemName: "pin.fill")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                }
                Text("via \(tunnel.activeJump ?? (tunnel.jumpCandidates?.first ?? "Auto"))")
                    .font(.rowMeta)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .help(tunnel.jumpCandidates == nil
              ? "Auto — any ready host. Click to pin."
              : "Pinned to \(tunnel.jumpCandidates!.joined(separator: ", ")). Click to change.")
    }

    // MARK: - Metadata column (aliveSince + fails)

    private var metadata: some View {
        HStack(spacing: Spacing.xs) {
            if isBusy {
                ProgressView()
                    .controlSize(.small)
                    .scaleEffect(0.6)
                    .frame(width: 12, height: 12)
                Text(busyLabel)
                    .font(.rowMeta)
                    .foregroundStyle(.orange)
                    .lineLimit(1)
                    .truncationMode(.tail)
            } else {
                if let aliveTxt = tunnel.aliveSince() {
                    Text(aliveTxt)
                        .font(.rowMeta)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                // Fail count — compact "3✗", tinted red/orange when > 0.
                if tunnel.failCount > 0 {
                    let failTint: Color = tunnel.failCount >= 3 ? .red : .orange
                    Text("\(tunnel.failCount)✗")
                        .font(.countBadge)
                        .foregroundStyle(failTint)
                        .lineLimit(1)
                        .help("\(tunnel.failCount) failed connection attempt\(tunnel.failCount == 1 ? "" : "s")")
                }
            }
        }
    }

    // MARK: - Actions (same calls / SF Symbols / disabled logic as old view)

    /// Hover-revealed icon+TEXT action bar — primary actions as one short word
    /// each. Same AppState calls + disabled logic as before; Details / Rename /
    /// Clone / jump-host / Delete live in the `⋯` overflow menu. Row height
    /// stays fixed.
    private var actions: some View {
        HStack(spacing: Spacing.xs) {
            // Start / Stop (toggle).
            Button {
                Task { await appState.toggleTunnel(tunnel) }
            } label: {
                if isBusy {
                    HStack(spacing: Spacing.xs) {
                        ProgressView().controlSize(.small).scaleEffect(0.6)
                            .frame(width: 14, height: 14)
                        Text(tunnel.displayState == .alive ? "Stop" : "Start")
                            .font(.caption)
                    }
                } else {
                    Label(tunnel.displayState == .alive ? "Stop" : "Start",
                          systemImage: tunnel.displayState == .alive ? "stop.fill" : "play.fill")
                }
            }
            .help(tunnel.displayState == .alive ? "Stop tunnel" : "Start tunnel")
            .accessibilityLabel(tunnel.displayState == .alive ? "Stop tunnel" : "Start tunnel")
            .disabled(isBusy)

            // Node (pick compute node).
            Button {
                appState.presentNodePicker(for: tunnel)
            } label: {
                Label("Node", systemImage: "list.bullet.rectangle")
            }
            .help("Pick compute node")
            .accessibilityLabel("Pick compute node")
            .disabled(isBusy)

            // Open in browser (disabled if not alive).
            Button {
                openInBrowser(tunnel)
            } label: {
                Label("Open", systemImage: "safari")
            }
            .help("Open in browser")
            .accessibilityLabel("Open in browser")
            .disabled(isBusy || tunnel.displayState != .alive)

            // Copy URL.
            Button {
                copyURL(tunnel.url)
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
            }
            .help("Copy localhost URL")
            .accessibilityLabel("Copy localhost URL")
        }
        .buttonStyle(IconTextActionButton())
    }

    // MARK: - Always-visible overflow menu (discoverable, labeled)

    /// Compact trailing `⋯` control that is ALWAYS visible (not hover-gated).
    /// Lists every row action as a TEXT-LABELED command — the discoverable,
    /// HIG-aligned path. Reuses the SAME action set as the TunnelsView
    /// right-click context menu and the same AppState calls as the inline icons.
    private var overflowMenu: some View {
        Menu {
            tunnelMenuItems
        } label: {
            Image(systemName: "ellipsis.circle")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .buttonStyle(.borderless)
        .fixedSize()
        .help("Actions")
        .accessibilityLabel("Tunnel actions")
    }

    /// Labeled action set mirroring the TunnelsView context menu. Same calls
    /// as the inline icons + the existing context menu.
    @ViewBuilder
    private var tunnelMenuItems: some View {
        Button {
            Task { await appState.toggleTunnel(tunnel) }
        } label: {
            Label(tunnel.displayState == .alive ? "Stop" : "Start",
                  systemImage: tunnel.displayState == .alive ? "stop.fill" : "play.fill")
        }
        .disabled(isBusy)

        Button {
            appState.presentNodePicker(for: tunnel)
        } label: {
            Label("Pick node…", systemImage: "list.bullet.rectangle")
        }
        .disabled(isBusy)

        Menu {
            jumpPickerMenu(for: tunnel)
        } label: {
            Label("Use jump host", systemImage: "arrow.triangle.branch")
        }

        Button {
            openInBrowser(tunnel)
        } label: {
            Label("Open in browser", systemImage: "safari")
        }
        .disabled(tunnel.displayState != .alive)

        Button {
            copyURL(tunnel.url)
        } label: {
            Label("Copy localhost:\(tunnel.localPort)", systemImage: "doc.on.doc")
        }

        Button {
            detailsForTunnel = tunnel
        } label: {
            Label("Details…", systemImage: "info.circle")
        }

        Divider()

        Button {
            renameDraft = tunnel.name
            renamingTunnel = tunnel
        } label: {
            Label("Rename…", systemImage: "pencil")
        }

        Button {
            Task { await appState.cloneTunnel(tunnel) }
        } label: {
            Label("Clone…", systemImage: "plus.square.on.square")
        }

        Divider()

        Button(role: .destructive) {
            appState.presentConfirmDelete(for: tunnel)
        } label: {
            Label("Delete", systemImage: "trash")
        }
        .disabled(isBusy)
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
