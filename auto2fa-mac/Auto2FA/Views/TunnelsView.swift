import SwiftUI

struct TunnelsView: View {
    @EnvironmentObject var appState: AppState
    @State private var selection: Tunnel.ID?

    var body: some View {
        if appState.tunnels.isEmpty {
            VStack(spacing: 12) {
                Image(systemName: "sparkles")
                    .font(.largeTitle)
                    .foregroundStyle(.yellow)
                Text("No tunnels yet")
                    .font(.title3)
                Text("Click  +  in the toolbar (or press ⌘N) to create one.")
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button {
                    appState.presentNewTunnel()
                } label: {
                    Label("New tunnel", systemImage: "plus")
                }
                .controlSize(.large)
                .keyboardShortcut(.defaultAction)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding()
        } else {
            Table(appState.tunnels, selection: $selection) {
                TableColumn("Name") { t in
                    Text(t.name).fontDesign(.monospaced)
                }
                .width(min: 80, ideal: 110)

                TableColumn("Local → Remote") { t in
                    Text(":\(t.localPort) → :\(t.remotePort)")
                        .fontDesign(.monospaced)
                        .foregroundStyle(.secondary)
                }
                .width(min: 100, ideal: 130)

                TableColumn("Node") { t in
                    if let n = t.lastNode {
                        Text(n).fontDesign(.monospaced).lineLimit(1)
                    } else {
                        Text("(no node yet)").foregroundStyle(.tertiary).italic()
                    }
                }

                TableColumn("Via") { t in
                    Text(t.activeJump ?? "—")
                        .foregroundStyle(.secondary)
                }
                .width(min: 60, ideal: 70)

                TableColumn("Status") { t in
                    HStack(spacing: 6) {
                        Circle()
                            .fill(color(for: t.displayState))
                            .frame(width: 8, height: 8)
                        Text(displayName(for: t.displayState))
                            .fontWeight(.medium)
                        Text(t.lastMsg)
                            .foregroundStyle(.secondary)
                            .font(.caption)
                            .lineLimit(1)
                    }
                }
                .width(min: 200)

                TableColumn("") { t in
                    HStack(spacing: 4) {
                        Button {
                            Task { await appState.toggleTunnel(t) }
                        } label: {
                            Image(systemName: t.displayState == .alive ? "stop.fill" : "play.fill")
                        }
                        .help(t.displayState == .alive ? "Stop" : "Start")
                        Button {
                            appState.presentNodePicker(for: t)
                        } label: {
                            Image(systemName: "list.bullet.rectangle")
                        }
                        .help("Pick a node from squeue")
                        Button {
                            copyURL(t.url)
                        } label: {
                            Image(systemName: "doc.on.doc")
                        }
                        .help("Copy localhost:\(t.localPort)")
                        Button {
                            appState.presentConfirmDelete(for: t)
                        } label: {
                            Image(systemName: "trash")
                        }
                        .help("Delete tunnel")
                    }
                    .buttonStyle(.borderless)
                }
                .width(min: 120, ideal: 140)
            }
            .contextMenu(forSelectionType: Tunnel.ID.self) { ids in
                if let id = ids.first,
                   let t = appState.tunnels.first(where: { $0.id == id }) {
                    Button(t.displayState == .alive ? "Stop" : "Start") {
                        Task { await appState.toggleTunnel(t) }
                    }
                    Button("Pick node…") {
                        appState.presentNodePicker(for: t)
                    }
                    Button("Copy localhost:\(t.localPort)") {
                        copyURL(t.url)
                    }
                    Divider()
                    Button("Delete tunnel", role: .destructive) {
                        appState.presentConfirmDelete(for: t)
                    }
                }
            }
        }
    }

    private func copyURL(_ url: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(url, forType: .string)
    }

    private func color(for state: Tunnel.DisplayState) -> Color {
        switch state {
        case .alive: return .green
        case .starting: return .yellow
        case .stale: return .red
        case .idle: return .secondary
        case .portBusy: return .red
        case .failed: return .red
        case .unknown: return .secondary
        }
    }

    private func displayName(for state: Tunnel.DisplayState) -> String {
        switch state {
        case .alive: return "Connected"
        case .starting: return "Connecting…"
        case .stale: return "Stale"
        case .idle: return "Idle"
        case .portBusy: return "Port busy"
        case .failed: return "Failed"
        case .unknown: return "—"
        }
    }
}
