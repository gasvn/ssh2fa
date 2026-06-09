import SwiftUI

/// Live-tail viewer for /tmp/auto2fa_daemon.log. Polls the daemon every 2s
/// via the log_tail IPC method, colorises common patterns, supports
/// substring filtering (case-insensitive). Bottom-pinned auto-scroll
/// unless the user has scrolled away.
struct LogViewerView: View {
    @EnvironmentObject var appState: AppState
    @State private var lines: [String] = []
    @State private var filter = ""
    @State private var autoScroll = true
    @State private var error: String?
    @State private var pollTask: Task<Void, Never>?

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
                TextField("filter (host name, error keyword, …)", text: $filter)
                    .textFieldStyle(.roundedBorder)
                Toggle("Auto-scroll", isOn: $autoScroll)
                    .toggleStyle(.checkbox)
                Button {
                    Task { await refreshOnce(lineCount: 1000) }
                } label: { Label("Reload", systemImage: "arrow.clockwise") }
                Button {
                    NSWorkspace.shared.activateFileViewerSelecting(
                        [URL(fileURLWithPath: "/tmp/auto2fa_daemon.log")])
                } label: { Label("Reveal", systemImage: "doc.text.magnifyingglass") }
                    .help("Show /tmp/auto2fa_daemon.log in Finder")
            }
            .padding(8)
            .background(.bar)

            Divider()

            if let error {
                Text(error)
                    .foregroundStyle(.red)
                    .padding(8)
            }

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(filtered.enumerated()), id: \.offset) { idx, line in
                            Text(highlight(line))
                                .font(.system(size: 11, design: .monospaced))
                                .textSelection(.enabled)
                                .lineLimit(nil)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 8)
                                .padding(.vertical, 1)
                                .background(idx.isMultiple(of: 2) ? Color.clear : Color.gray.opacity(0.05))
                                .id(idx)
                        }
                    }
                    .padding(.vertical, 4)
                }
                .background(Color.black.opacity(0.02))
                .onChange(of: filtered.count) { _, _ in
                    if autoScroll, !filtered.isEmpty {
                        proxy.scrollTo(filtered.count - 1, anchor: .bottom)
                    }
                }
            }
        }
        .frame(minWidth: 700, minHeight: 400)
        .task {
            await refreshOnce(lineCount: 500)
            startPolling()
        }
        .onDisappear {
            pollTask?.cancel()
        }
    }

    private var filtered: [String] {
        let q = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return lines }
        return lines.filter { $0.lowercased().contains(q) }
    }

    private func startPolling() {
        pollTask?.cancel()
        pollTask = Task { [weak appState] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard let state = appState else { return }
                do {
                    let fresh = try await state.client.logTail(lines: 500)
                    await MainActor.run {
                        self.lines = fresh
                        // Clear a stale error once polling recovers — one
                        // transient failure used to leave a permanent red
                        // banner above a perfectly live log.
                        self.error = nil
                    }
                } catch {
                    // Daemon may have died — surface but keep trying
                    await MainActor.run {
                        self.error = error.localizedDescription
                    }
                }
            }
        }
    }

    private func refreshOnce(lineCount: Int) async {
        do {
            let fresh = try await appState.client.logTail(lines: lineCount)
            await MainActor.run {
                self.lines = fresh
                self.error = nil
            }
        } catch {
            await MainActor.run {
                self.error = "Could not read log: \(error.localizedDescription)"
            }
        }
    }

    /// Tint important lines. Returns an AttributedString so we can apply
    /// foreground per line. Cheap: just one keyword scan.
    private func highlight(_ line: String) -> AttributedString {
        var attr = AttributedString(line)
        let lower = line.lowercased()
        if lower.contains("error") || lower.contains("failed") || lower.contains("crashed") {
            attr.foregroundColor = .red
        } else if lower.contains("warning") {
            attr.foregroundColor = .orange
        } else if lower.contains("ready") || lower.contains("connected") || lower.contains("alive") {
            attr.foregroundColor = .green
        } else if lower.contains("info") {
            attr.foregroundColor = .secondary
        }
        return attr
    }
}
