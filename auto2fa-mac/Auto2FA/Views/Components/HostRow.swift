import SwiftUI

/// Two-line, native-minimal row for a single SSH host.
///
/// Line 1: status badge · alias (mono) · pool pips · mount indicator ·
///         hover actions (start/stop, mount/eject, rotate, terminal).
/// Line 2: friendly last message (only when meaningful); tooltip = raw msg.
///
/// All actions route through the shared `AppState` (same calls as the old
/// `Table`-based `HostsView`). Presentation only — zero functional change.
struct HostRow: View {
    let host: SSHHost
    @EnvironmentObject var appState: AppState

    @State private var hovering = false

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

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            line1
            if !isBusy, !friendlyMessage.isEmpty {
                Text(friendlyMessage)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .help(host.lastMsg)
                    .padding(.leading, RowMetric.iconSize + Spacing.xs)
            }
        }
        .dashboardRow()
        .contentShape(Rectangle())
        .changeHighlight(host.status)
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
                .layoutPriority(1)
            } else {
                StatusBadge(host: host.displayState,
                            text: FriendlyText.hostStatus(host.status))
                    .layoutPriority(1)
            }

            // Alias (mono, primary).
            Text(host.host)
                .font(RowMetric.mono)
                .foregroundStyle(.primary)
                .lineLimit(1)
                .truncationMode(.middle)

            // Pool pips: poolAlive filled of 2, rest hollow.
            if !isBusy {
                poolPips
            }

            Spacer(minLength: Spacing.s)

            // Mount indicator (green when mounted; hidden otherwise).
            if host.isMounted {
                Image(systemName: "externaldrive.connected.to.line.below.fill")
                    .foregroundStyle(.green)
                    .help("Remote filesystem mounted")
            }

            // Hover actions.
            if hovering {
                actions
                    .transition(.opacity)
            }
        }
    }

    private var poolPips: some View {
        HStack(spacing: 2) {
            ForEach(0..<2, id: \.self) { i in
                Image(systemName: i < host.poolAlive ? "circle.fill" : "circle")
                    .font(.system(size: 6))
                    .foregroundStyle(i < host.poolAlive ? Color.green : Color.secondary)
            }
        }
        .help("\(host.poolAlive)/2 connections ready")
    }

    // MARK: - Actions (same calls / disabled logic as old HostsView)

    private var actions: some View {
        HStack(spacing: Spacing.xs) {
            // Start / stop (toggle active).
            Button {
                Task { await appState.toggleHost(host) }
            } label: {
                if isBusy {
                    ProgressView()
                        .controlSize(.small)
                        .scaleEffect(0.6)
                        .frame(width: 14, height: 14)
                } else {
                    Image(systemName: host.active ? "stop.fill" : "play.fill")
                }
            }
            .help(host.active ? "Stop / disconnect" : "Start / connect")
            .disabled(isBusy)

            // Mount / eject.
            Button {
                Task { await appState.toggleMount(host) }
            } label: {
                Image(systemName: host.isMounted ? "eject.fill" : "externaldrive.badge.plus")
            }
            .disabled(isBusy || (!host.isMasterReady && !host.isMounted))
            .help(host.isMounted ? "Unmount remote filesystem" : "Mount remote filesystem (sshfs)")

            // Rotate pool slot.
            Button {
                Task { await appState.rotateHost(host) }
            } label: {
                Image(systemName: "arrow.triangle.2.circlepath")
            }
            .disabled(isBusy || !host.active)
            .help("Rotate connection pool slot")

            // Open terminal.
            Button {
                openTerminal(for: host)
            } label: {
                Image(systemName: "terminal")
            }
            .disabled(!host.isMasterReady)
            .help("Open an interactive ssh session in Terminal")
        }
        .buttonStyle(.borderless)
    }

    // MARK: - Terminal (verbatim from old HostsView)

    /// Pop a Terminal.app window running `ssh <host>` over the warm
    /// ControlMaster — instantaneous, no 2FA prompt.
    private func openTerminal(for host: SSHHost) {
        let script = """
        tell application "Terminal"
            activate
            do script "ssh \(host.host)"
        end tell
        """
        var error: NSDictionary?
        if let scriptObj = NSAppleScript(source: script) {
            scriptObj.executeAndReturnError(&error)
            if let error {
                NSLog("[Auto2FA] openTerminal AppleScript error: \(error)")
            }
        }
    }
}
