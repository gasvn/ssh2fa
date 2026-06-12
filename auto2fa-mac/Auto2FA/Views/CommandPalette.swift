import SwiftUI

/// Spotlight-style command palette. ⌘⇧P opens it. Type to filter through
/// every action the app exposes:
///   - start / stop / open browser / open terminal / copy URL for each tunnel
///   - open terminal for each host
///   - global actions (add tunnel, add host, reset, logs, settings, wake)
///
/// Arrow keys move selection, Enter runs, Esc dismisses. Cuts the typical
/// "find the right row → right-click → submenu" loop to a single sentence.
struct CommandPalette: View {
    @EnvironmentObject var appState: AppState
    @Environment(\.dismiss) private var dismiss
    // Native SwiftUI window opener — works for any registered WindowGroup id.
    // Previously we tried `NSApp.sendAction(Selector(("openLogsWindow:")))`
    // which doesn't exist anywhere → "Show daemon logs" silently no-op'd
    // unless a logs window happened to already be open.
    @Environment(\.openWindow) private var openWindow
    @State private var query: String = ""
    @State private var selectedIdx: Int = 0
    @FocusState private var focused: Bool

    private var commands: [PaletteCommand] {
        var out: [PaletteCommand] = []

        // Per-tunnel actions
        for t in appState.tunnels {
            let alive = t.displayState == .alive
            out.append(.init(
                icon: alive ? "stop.fill" : "play.fill",
                title: "\(alive ? "Stop" : "Start") tunnel — \(t.name)",
                subtitle: "localhost:\(t.localPort) → \(t.lastNode ?? "?")",
                keywords: [t.name, "tunnel", alive ? "stop" : "start"] + t.tags
            ) { Task { await appState.toggleTunnel(t) } })
            out.append(.init(
                icon: "list.bullet.rectangle",
                title: "Pick node — \(t.name)",
                subtitle: "browse SLURM jobs on jump host",
                keywords: [t.name, "node", "squeue", "pick"]
            ) { appState.presentNodePicker(for: t) })
            if alive {
                // Use browserURL (includes the tunnel's url_path, e.g. a
                // jupyter "?token=…") — the raw localhost URL dropped the
                // token and landed on a login page.
                out.append(.init(
                    icon: "safari",
                    title: "Open in browser — \(t.name)",
                    subtitle: t.browserURL,
                    keywords: [t.name, "open", "browser", "safari"]
                ) {
                    if let url = URL(string: t.browserURL) {
                        NSWorkspace.shared.open(url)
                    }
                })
            }
            out.append(.init(
                icon: "doc.on.doc",
                title: "Copy URL — \(t.name)",
                subtitle: "localhost:\(t.localPort)",
                keywords: [t.name, "copy", "url", "clipboard"]
            ) {
                let pb = NSPasteboard.general
                pb.clearContents()
                pb.setString(t.url, forType: .string)
                FriendlyText.haptic()
            })
            out.append(.init(
                icon: "doc.on.doc.fill",
                title: "Clone — \(t.name)",
                subtitle: "duplicate with next free port",
                keywords: [t.name, "clone", "duplicate", "copy"]
            ) { Task { await appState.cloneTunnel(t) } })
        }

        // Per-host actions
        for host in appState.hosts where host.isMasterReady {
            out.append(.init(
                icon: "terminal",
                title: "Open Terminal — \(host.host)",
                subtitle: "ssh \(host.host) via warm ControlMaster",
                keywords: [host.host, "terminal", "ssh", "shell"]
            ) {
                // Escape for the AppleScript literal (defense-in-depth; the
                // daemon validates host names at add time).
                let safeHost = host.host
                    .replacingOccurrences(of: "\\", with: "\\\\")
                    .replacingOccurrences(of: "\"", with: "\\\"")
                let script = "tell application \"Terminal\"\n  activate\n  do script \"ssh \(safeHost)\"\nend tell"
                NSAppleScript(source: script)?.executeAndReturnError(nil)
            })
        }

        // Global
        out.append(contentsOf: [
            .init(icon: "plus.circle.fill",
                  title: "New tunnel…",
                  subtitle: "open the create-tunnel wizard",
                  keywords: ["new", "tunnel", "create", "add"]
            ) { appState.presentNewTunnel() },
            .init(icon: "server.rack",
                  title: "Add SSH host…",
                  subtitle: "register a new host with 2FA",
                  keywords: ["new", "host", "add", "ssh", "2fa"]
            ) { appState.presentAddHost() },
            .init(icon: "arrow.triangle.2.circlepath",
                  title: "Wake recover",
                  subtitle: "probe + rebuild dead masters, restart tunnels",
                  keywords: ["wake", "recover", "reconnect"]
            ) {
                Task { try? await appState.client.wakeRecover(); await appState.reloadAll() }
            },
            .init(icon: "exclamationmark.arrow.circlepath",
                  title: "Reset everything",
                  subtitle: "nuclear — stops all tunnels + rebuilds all masters",
                  keywords: ["reset", "nuke", "panic", "restart"]
            ) { Task { await appState.resetAll() } },
            .init(icon: "doc.text.magnifyingglass",
                  title: "Show daemon logs…",
                  subtitle: "live tail with filter",
                  keywords: ["logs", "log", "daemon", "debug"]
            ) {
                NSApp.activate(ignoringOtherApps: true)
                // First check if a logs window is already open and focus it.
                if let w = NSApp.windows.first(where: { $0.title == "SSH2FA Logs" }) {
                    w.makeKeyAndOrderFront(nil)
                    return
                }
                // Otherwise actually open the registered "logs" WindowGroup.
                openWindow(id: "logs")
            },
            .init(icon: "gear",
                  title: "Settings…",
                  subtitle: "preferences (⌘,)",
                  keywords: ["settings", "preferences", "options"]
            ) {
                NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
            },
        ])
        return out
    }

    private var filtered: [PaletteCommand] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return commands }
        return commands.filter { cmd in
            if cmd.title.lowercased().contains(q) { return true }
            if cmd.subtitle.lowercased().contains(q) { return true }
            if cmd.keywords.contains(where: { $0.lowercased().contains(q) }) { return true }
            return false
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
                TextField("Type a command…", text: $query)
                    .textFieldStyle(.plain)
                    .font(.title2)
                    .focused($focused)
                    .onSubmit { runSelected() }
                    .onChange(of: query) { _, _ in selectedIdx = 0 }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)

            Divider()

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        if filtered.isEmpty {
                            Text("No matches.")
                                .foregroundStyle(.secondary)
                                .padding()
                        } else {
                            ForEach(Array(filtered.enumerated()), id: \.element.id) { idx, cmd in
                                row(cmd, isSelected: idx == selectedIdx)
                                    .id(idx)
                                    .onTapGesture {
                                        runCommand(cmd)
                                    }
                            }
                        }
                    }
                }
                .frame(maxHeight: 400)
                .onChange(of: selectedIdx) { _, new in
                    withAnimation(.easeOut(duration: 0.1)) {
                        proxy.scrollTo(new, anchor: .center)
                    }
                }
            }
        }
        .frame(width: 600)
        .glassChrome(cornerRadius: Radius.card)
        .onAppear { focused = true }
        .onKeyPress(.downArrow) {
            selectedIdx = min(filtered.count - 1, selectedIdx + 1)
            return .handled
        }
        .onKeyPress(.upArrow) {
            selectedIdx = max(0, selectedIdx - 1)
            return .handled
        }
        .onKeyPress(.escape) {
            dismiss()
            return .handled
        }
    }

    @ViewBuilder
    private func row(_ cmd: PaletteCommand, isSelected: Bool) -> some View {
        HStack(spacing: 12) {
            Image(systemName: cmd.icon)
                .font(.title3)
                .foregroundColor(isSelected ? .white : .accentColor)
                .frame(width: 24, alignment: .center)
            VStack(alignment: .leading, spacing: 1) {
                Text(cmd.title)
                    .font(.body)
                    .foregroundColor(isSelected ? .white : .primary)
                Text(cmd.subtitle)
                    .font(.caption)
                    .foregroundColor(isSelected ? .white.opacity(0.8) : .secondary)
                    .lineLimit(1)
            }
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .glassEffect(isSelected ? .regular.tint(.accentColor).interactive() : .identity,
                     in: .rect(cornerRadius: Radius.control, style: .continuous))
        .contentShape(Rectangle())
    }

    private func runSelected() {
        let list = filtered
        guard !list.isEmpty, selectedIdx < list.count else { return }
        runCommand(list[selectedIdx])
    }

    private func runCommand(_ cmd: PaletteCommand) {
        dismiss()
        // Run on next runloop tick so dismiss animation completes first.
        DispatchQueue.main.async { cmd.action() }
    }
}

struct PaletteCommand: Identifiable {
    // Stable identity: `commands` is a computed property re-built on every
    // render, so a fresh UUID() per command made every row's identity churn
    // per keystroke (full ForEach rebuild + lost row state). Titles are unique
    // within the palette.
    var id: String { title }
    let icon: String
    let title: String
    let subtitle: String
    let keywords: [String]
    let action: () -> Void
}
