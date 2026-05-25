import SwiftUI

struct HostsView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        Table(appState.hosts) {
            TableColumn("Host") { host in
                Text(host.host)
                    .fontDesign(.monospaced)
            }
            .width(min: 100, ideal: 140)

            TableColumn("Status") { host in
                HStack(spacing: 6) {
                    if isBusy(host) {
                        // Spinner covers both the brief RPC round-trip AND
                        // the long server-side login (the toggle RPC returns
                        // almost instantly, but the daemon may then spend
                        // 20-40s authenticating). Either source = spinning.
                        ProgressView()
                            .controlSize(.small)
                            .scaleEffect(0.7)
                        Text(busyLabel(host))
                            .foregroundStyle(.orange)
                    } else {
                        Circle()
                            .fill(color(for: host.displayState))
                            .frame(width: 8, height: 8)
                        Text(displayName(for: host.displayState))
                        if host.poolAlive > 0 {
                            Text("(\(host.poolIndex)/\(host.poolAlive))")
                                .foregroundStyle(.secondary)
                                .font(.caption)
                        }
                    }
                }
            }
            .width(min: 140, ideal: 200)

            TableColumn("FS") { host in
                Image(systemName: host.isMounted ? "externaldrive.connected.to.line.below.fill" : "externaldrive")
                    .foregroundStyle(host.isMounted ? .green : .secondary)
            }
            .width(min: 40, ideal: 50, max: 60)

            TableColumn("Last Message") { host in
                Text(host.lastMsg)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            TableColumn("") { host in
                let busy = isBusy(host)
                HStack(spacing: 4) {
                    Button {
                        Task { await appState.toggleHost(host) }
                    } label: {
                        if busy {
                            ProgressView().controlSize(.small).scaleEffect(0.6)
                                .frame(width: 14, height: 14)
                        } else {
                            Image(systemName: host.active ? "stop.fill" : "play.fill")
                        }
                    }
                    .help(host.active ? "Stop / disconnect" : "Start / connect")
                    .disabled(busy)
                    Button {
                        Task { await appState.toggleMount(host) }
                    } label: {
                        Image(systemName: host.isMounted ? "eject.fill" : "externaldrive.badge.plus")
                    }
                    .disabled(busy || (!host.isMasterReady && !host.isMounted))
                    .help(host.isMounted ? "Unmount remote filesystem" : "Mount remote filesystem (sshfs)")
                    Button {
                        Task { await appState.rotateHost(host) }
                    } label: {
                        Image(systemName: "arrow.triangle.2.circlepath")
                    }
                    .disabled(busy || !host.active)
                    .help("Rotate connection pool slot")
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
            .width(min: 130, ideal: 140, max: 170)
        }
    }

    /// Treat both the click-feedback flag AND the daemon-reported
    /// "connecting" state as "busy". This avoids the spinner flickering
    /// for ~50ms only — the actual login takes ~20s after the toggle RPC
    /// returns, and during that whole window we want to show progress.
    private func isBusy(_ host: SSHHost) -> Bool {
        if appState.inFlightHosts.contains(host.host) { return true }
        return host.displayState == .connecting
    }

    private func busyLabel(_ host: SSHHost) -> String {
        // last_msg is whatever the daemon last set ("Init Spawn #0...",
        // "Spawning #0...", etc.) — usually more useful than a generic
        // "Connecting…" because it tells the user how far they are.
        let msg = host.lastMsg.trimmingCharacters(in: .whitespacesAndNewlines)
        return msg.isEmpty ? "Working…" : msg
    }

    /// Pop a Terminal.app window running `ssh <host>` so the user can
    /// jump into an interactive session over the same ControlMaster the
    /// app keeps warm. The mux makes this instantaneous — no 2FA prompt.
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

    private func color(for state: SSHHost.DisplayState) -> Color {
        switch state {
        case .connected: return .green
        case .connecting: return .yellow
        case .failed: return .red
        case .stopped: return .secondary
        case .unknown: return .secondary
        }
    }

    private func displayName(for state: SSHHost.DisplayState) -> String {
        switch state {
        case .connected: return "Connected"
        case .connecting: return "Connecting…"
        case .failed: return "Failed"
        case .stopped: return "Stopped"
        case .unknown: return "Unknown"
        }
    }
}
