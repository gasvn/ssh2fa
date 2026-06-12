import SwiftUI

/// Single-line, dense row for a single SSH host — aligned columns like a
/// clean compact table.
///
/// `[dot] alias  hostname  poolPips  [fsIcon]  [TOTPCodeChip]  <Spacer>  [hover actions]`
///
/// The friendly last-message lives in the row's `.help(...)` tooltip so the
/// row stays one line but the info is still accessible. All actions route
/// through the shared `AppState` (same calls as before) — presentation only,
/// zero functional change.
struct HostRow: View {
    let host: SSHHost
    @EnvironmentObject var appState: AppState

    @State private var hovering = false
    @Namespace private var actionGlassNS

    // MARK: - Busy logic (verbatim from old HostsView)

    /// Treat both the click-feedback flag AND the daemon-reported
    /// "connecting" state as "busy" — the toggle RPC returns in ~50ms but
    /// the actual login takes ~20-40s, and we want a spinner the whole time.
    private var isBusy: Bool {
        if appState.inFlightHosts.contains(host.host) { return true }
        return host.displayState == .connecting
    }

    private var busyLabel: String {
        let msg = host.lastMsg.trimmingCharacters(in: .whitespacesAndNewlines)
        return msg.isEmpty ? "Working…" : msg
    }

    private var friendlyMessage: String {
        FriendlyText.hostLastMsg(host.lastMsg)
    }

    /// Tooltip text for the whole row — the friendly last-message (moved off
    /// the row to keep it single-line), falling back to status.
    private var rowTooltip: String {
        let f = friendlyMessage
        if !f.isEmpty { return f }
        return FriendlyText.hostStatus(host.status)
    }

    var body: some View {
        HStack(spacing: Spacing.s) {
            // Leading status dot (compact — not the wide pill). Pulses while
            // busy via the .connecting display state.
            StatusDot(host: host.displayState)
                .frame(width: RowMetric.iconSize, height: RowMetric.iconSize)

            // Alias (rounded title, primary) — fixed-ish leading column so the
            // following columns line up across rows.
            Text(host.host)
                .font(.rowTitle)
                .foregroundStyle(.primary)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(minWidth: 56, alignment: .leading)

            // Spinner + progress text while busy, else flexible hostname column.
            if isBusy {
                HStack(spacing: Spacing.xs) {
                    ProgressView()
                        .controlSize(.small)
                        .scaleEffect(0.6)
                        .frame(width: 12, height: 12)
                    Text(busyLabel)
                        .font(.rowMeta)
                        .foregroundStyle(.orange)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                // Resolved hostname (secondary), flexible column. The model only
                // carries the alias (`host.host`); there is no separate resolved
                // name, so this is blank but the column still reserves flexible
                // width to keep the following columns aligned across rows.
                Text(hostnameText)
                    .font(.rowIdentifier)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }

            // Pool pips (filled/hollow) — fixed small. Hidden on hover so the
            // action bar can take the trailing zone (kept at rest).
            if !isBusy && !hovering {
                poolPips
            }

            // Mount indicator (green when mounted; hidden otherwise) — fixed.
            // Hidden on hover (the action bar shows Mount/Unmount instead).
            if host.isMounted && !hovering {
                Image(systemName: "externaldrive.connected.to.line.below.fill")
                    .font(.system(size: 11))
                    .foregroundStyle(.green)
                    .help("Remote filesystem mounted")
            }

            // Live 2FA (TOTP) code chip — compact, kept verbatim. Stays
            // visible at rest AND on hover (reveal-on-tap behaviour intact).
            TOTPCodeChip(host: host.host)

            Spacer(minLength: Spacing.s)

            // TRAILING ZONE: at rest the metadata above is shown; on hover a
            // right-aligned icon+TEXT action bar (primary actions) + a labeled
            // `⋯` overflow menu replaces it. Row height stays fixed.
            if hovering {
                actions
                    .transition(.opacity)
                overflowMenu
                    .transition(.opacity)
            }
        }
        .padding(.vertical, 2)
        .frame(minHeight: RowMetric.minHeight)
        .contentShape(Rectangle())
        .help(rowTooltip)
        .changeHighlight(host.status)
        .hoverLift(hovering)
        .onHover { h in withAnimation(.bouncy(duration: 0.35)) { hovering = h } }
        .contextMenu { hostMenuItems }
    }

    // MARK: - Hostname column

    /// The resolved hostname, if present. The model only carries `host` (the
    /// SSH alias), so there is no distinct resolved name to show — blank.
    private var hostnameText: String { "" }

    /// Single-master: one connection per host — a single dot (filled green when
    /// the master is ready, hollow otherwise). Replaces the old 2-slot pool pips.
    private var poolPips: some View {
        Image(systemName: host.isMasterReady ? "circle.fill" : "circle")
            .font(.system(size: 6))
            .foregroundStyle(host.isMasterReady ? Color.green : Color.secondary)
            .help(host.isMasterReady ? "Connected" : "Not connected")
    }

    // MARK: - Actions (same calls / disabled logic as old HostsView)

    /// Hover-revealed icon+TEXT action bar — primary actions as one short word
    /// each. Same AppState calls + disabled logic as before; Rotate lives in
    /// the `⋯` overflow menu. Row height stays fixed.
    private var actions: some View {
        GlassEffectContainer(spacing: Spacing.xs) {
            HStack(spacing: Spacing.xs) {
                glassActionButton(id: "toggle",
                                  disabled: appState.inFlightHosts.contains(host.host),
                                  help: host.active ? "Disconnect host" : "Connect host") {
                    Task { await appState.toggleHost(host) }
                } label: {
                    if isBusy {
                        HStack(spacing: Spacing.xs) {
                            ProgressView().controlSize(.small).scaleEffect(0.6)
                                .frame(width: 14, height: 14)
                            Text(host.active ? "Disconnect" : "Connect").font(.caption)
                        }
                    } else {
                        Label(host.active ? "Disconnect" : "Connect",
                              systemImage: host.active ? "stop.fill" : "play.fill")
                    }
                }

                glassActionButton(id: "mount",
                                  disabled: isBusy || (!host.isMasterReady && !host.isMounted),
                                  help: host.isMounted ? "Unmount filesystem" : "Mount filesystem") {
                    Task { await appState.toggleMount(host) }
                } label: {
                    Label(host.isMounted ? "Unmount" : "Mount",
                          systemImage: host.isMounted ? "eject.fill" : "externaldrive.badge.plus")
                }

                glassActionButton(id: "terminal",
                                  disabled: !host.isMasterReady,
                                  help: "Open Terminal") {
                    openTerminal(for: host)
                } label: {
                    Label("Terminal", systemImage: "terminal")
                }
            }
        }
    }

    /// One morphing glass pill in the hover action bar. `id` ties it to the row's
    /// GlassEffectContainer namespace so pills fluidly merge/split on hover.
    @ViewBuilder
    private func glassActionButton<L: View>(
        id: String,
        disabled: Bool,
        help: String,
        action: @escaping () -> Void,
        @ViewBuilder label: () -> L
    ) -> some View {
        Button(action: action, label: label)
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
    /// Opens a menu where every row action is a TEXT-LABELED command — the
    /// discoverable, HIG-aligned path. Mirrors the inline icons + same calls.
    private var overflowMenu: some View {
        Menu {
            hostMenuItems
        } label: {
            Image(systemName: "ellipsis.circle")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .buttonStyle(.borderless)
        .fixedSize()
        .help("Actions")
        .accessibilityLabel("Host actions")
    }

    /// Shared labeled action set — used by BOTH the `⋯` overflow menu and the
    /// row's right-click context menu. Same AppState calls + disabled logic as
    /// the inline icons.
    @ViewBuilder
    private var hostMenuItems: some View {
        Button {
            Task { await appState.toggleHost(host) }
        } label: {
            Label(host.active ? "Disconnect" : "Connect",
                  systemImage: host.active ? "stop.fill" : "play.fill")
        }
        // Always allow stopping an active host (see inline actions note) —
        // only block during the brief in-flight toggle RPC.
        .disabled(appState.inFlightHosts.contains(host.host))

        Button {
            Task { await appState.toggleMount(host) }
        } label: {
            Label(host.isMounted ? "Unmount" : "Mount filesystem",
                  systemImage: host.isMounted ? "eject.fill" : "externaldrive.badge.plus")
        }
        .disabled(isBusy || (!host.isMasterReady && !host.isMounted))

        Button {
            Task { await appState.rotateHost(host) }
        } label: {
            Label("Rotate connection", systemImage: "arrow.triangle.2.circlepath")
        }
        .disabled(isBusy || !host.active)

        Button {
            openTerminal(for: host)
        } label: {
            Label("Open Terminal", systemImage: "terminal")
        }
        .disabled(!host.isMasterReady)
    }

    // MARK: - Terminal (verbatim from old HostsView)

    /// Pop a Terminal.app window running `ssh <host>` over the warm
    /// ControlMaster — instantaneous, no 2FA prompt.
    private func openTerminal(for host: SSHHost) {
        // Escape for the AppleScript string literal (defense-in-depth — the
        // daemon restricts host names to [A-Za-z0-9._-] at add time, but a
        // hand-edited passwords.json could contain a quote that would break
        // out of the literal).
        let safeHost = host.host
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let script = """
        tell application "Terminal"
            activate
            do script "ssh \(safeHost)"
        end tell
        """
        var error: NSDictionary?
        if let scriptObj = NSAppleScript(source: script) {
            scriptObj.executeAndReturnError(&error)
            if let error {
                NSLog("[SSH2FA] openTerminal AppleScript error: \(error)")
            }
        }
    }
}
