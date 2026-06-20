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
    @Namespace private var actionGlassNS

    // MARK: - Busy logic (verbatim from old TunnelsView)

    /// Busy = we just clicked something (inFlightTunnels) OR the daemon is
    /// reporting starting.
    private var isBusy: Bool {
        if appState.inFlightTunnels.contains(tunnel.name) { return true }
        return tunnel.displayState == .starting
    }

    /// The tunnel is "on" — running OR trying to (alive/starting). Used so the
    /// toggle shows a STOP affordance whenever the tunnel is up or attempting,
    /// not only when fully alive (otherwise a tunnel stuck "starting" forever
    /// could never be stopped).
    private var tunnelIsOn: Bool {
        tunnel.displayState == .alive || tunnel.displayState == .starting
    }

    /// A failure state where the user needs a recovery action (Retry / re-pick node).
    private var isFailedState: Bool {
        switch tunnel.displayState {
        case .stale, .portBusy, .failed: return true
        default: return false
        }
    }

    private func countdownColor(_ remaining: TimeInterval) -> Color {
        if remaining < 300 { return .red }      // < 5 min (incl. expired)
        if remaining < 1800 { return .orange }  // < 30 min
        return .secondary
    }

    /// Live compute-allocation countdown (SLURM walltime remaining).
    @ViewBuilder
    private func countdownView(endsAt: Date) -> some View {
        TimelineView(.periodic(from: .now, by: 1)) { ctx in
            let remaining = endsAt.timeIntervalSince(ctx.date)
            Label(SlurmTime.format(remaining: remaining), systemImage: "hourglass")
                .labelStyle(.titleAndIcon)
                .font(.rowMeta)
                .foregroundStyle(countdownColor(remaining))
                .lineLimit(1)
                .help("Compute allocation time left")
        }
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

            // Ports :local → :remote — sized to its content so the ports are
            // NEVER clipped. u16 ports can be 5 digits each (":65535 → :65535"),
            // which overflowed the old fixed 110pt column and clipped the remote
            // port's tail. fixedSize lets it take its natural width at any font
            // size; minWidth keeps short ports aligned with the column.
            Text(":\(tunnel.localPort) → :\(tunnel.remotePort)")
                .font(.rowIdentifier)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
                .frame(minWidth: 110, alignment: .leading)

            // Node + via + metadata — shown at rest, hidden on hover OR when the
            // tunnel is failed (so the action bar / recovery buttons get the full
            // trailing width without clipping).
            if !hovering && !isFailedState {
                // Target column: direct → "→ host"; compute → node or "(no node)".
                Group {
                    if tunnel.isDirect {
                        Text("→ \(tunnel.directHost ?? "host")")
                            .font(.rowIdentifier)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    } else if let n = tunnel.lastNode {
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

                // direct → static "direct" label; compute → clickable jump menu.
                Group {
                    if tunnel.isDirect {
                        Text("direct")
                            .font(.rowMeta)
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                    } else {
                        viaMenu
                    }
                }
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
            } else if isFailedState {
                // Failed → recovery actions at rest: Retry + re-pick Node.
                Button { Task { await appState.toggleTunnel(tunnel) } } label: {
                    Label("Retry", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.glass).controlSize(.small)
                .disabled(appState.inFlightTunnels.contains(tunnel.name))
                .transition(.opacity)
                // A node pick can't free a busy LOCAL port, so don't offer it
                // for portBusy (the fix is a different local port).
                if tunnel.displayState != .portBusy && !tunnel.isDirect {
                    Button { appState.presentNodePicker(for: tunnel) } label: {
                        Label("Node", systemImage: "list.bullet.rectangle")
                    }
                    .buttonStyle(.glass).controlSize(.small)
                    .disabled(appState.inFlightTunnels.contains(tunnel.name))
                    .transition(.opacity)
                }
            }
        }
        .padding(.vertical, compactRows ? 1 : 2)
        .frame(minHeight: compactRows ? 22 : RowMetric.minHeight)
        .contentShape(Rectangle())
        // Flash yellow briefly whenever the tunnel's status string changes —
        // helps the eye catch quick transitions like starting → alive.
        .changeHighlight(tunnel.status)
        .help(rowTooltip)
        .hoverLift(hovering)
        .onHover { h in withAnimation(.bouncy(duration: 0.35)) { hovering = h } }
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
        .help((tunnel.jumpCandidates?.isEmpty ?? true)
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
                if tunnelIsOn, let endsAt = TunnelDeadlines.endsAt(tunnel.name) {
                    countdownView(endsAt: endsAt)
                } else if let aliveTxt = tunnel.aliveSince() {
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
        GlassEffectContainer(spacing: Spacing.xs) {
            HStack(spacing: Spacing.xs) {
                glassActionButton(id: "toggle",
                                  disabled: appState.inFlightTunnels.contains(tunnel.name),
                                  help: tunnelIsOn ? "Stop tunnel" : "Start tunnel") {
                    Task { await appState.toggleTunnel(tunnel) }
                } label: {
                    if isBusy {
                        HStack(spacing: Spacing.xs) {
                            ProgressView().controlSize(.small).scaleEffect(0.6)
                                .frame(width: 14, height: 14)
                            Text(tunnelIsOn ? "Stop" : "Start").font(.caption)
                        }
                    } else {
                        Label(tunnelIsOn ? "Stop" : "Start",
                              systemImage: tunnelIsOn ? "stop.fill" : "play.fill")
                    }
                }

                if !tunnel.isDirect {
                    glassActionButton(id: "node",
                                      disabled: isBusy,
                                      help: "Pick compute node") {
                        appState.presentNodePicker(for: tunnel)
                    } label: {
                        Label("Node", systemImage: "list.bullet.rectangle")
                    }
                }

                glassActionButton(id: "open",
                                  disabled: isBusy || tunnel.displayState != .alive,
                                  help: "Open in browser") {
                    openInBrowser(tunnel)
                } label: {
                    Label("Open", systemImage: "safari")
                }

                glassActionButton(id: "copy",
                                  disabled: false,
                                  help: "Copy localhost URL") {
                    copyURL(tunnel.url)
                } label: {
                    Label("Copy", systemImage: "doc.on.doc")
                }
            }
        }
    }

    /// One morphing glass pill in the hover action bar (mirrors HostRow).
    @ViewBuilder
    private func glassActionButton<L: View>(
        id: String,
        disabled: Bool,
        help: String,
        action: @escaping () -> Void,
        @ViewBuilder label: () -> L
    ) -> some View {
        Button(action: action, label: label)
            .labelStyle(.titleAndIcon)
            .buttonStyle(.plain)
            .font(.caption)
            .padding(.horizontal, 8)
            .frame(height: 22)
            .foregroundStyle(disabled ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.primary))
            .glassEffect(.regular.interactive(), in: .capsule)
            .glassEffectID(id, in: actionGlassNS)
            .disabled(disabled)
            .help(help)
            .accessibilityLabel(help)
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
            Label(tunnelIsOn ? "Stop" : "Start",
                  systemImage: tunnelIsOn ? "stop.fill" : "play.fill")
        }
        .disabled(appState.inFlightTunnels.contains(tunnel.name))

        if !tunnel.isDirect {
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
