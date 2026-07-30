import SwiftUI

/// Manage the remote folders pinned for one host's sshfs mount.
///
/// The mount used to be all-or-nothing: it always mounted `/`, so every session
/// began by navigating back down to whatever directory you actually work in.
/// Pinning a folder mounts it directly — and the first pin becomes what plain
/// "Mount" uses.
struct MountFoldersSheet: View {
    let hostName: String
    @EnvironmentObject var appState: AppState

    @State private var pathDraft = ""
    @State private var labelDraft = ""
    @State private var error: String?
    @FocusState private var pathFocused: Bool
    // Remote browser: pinning a folder should be a matter of clicking to it,
    // not recalling an absolute path.
    @State private var browsing = false
    @State private var browsePath = "/"
    @State private var browseEntries: [BackendClient.RemoteDir] = []
    @State private var browseLoading = false
    @State private var browseError: String?

    private var bookmarks: [MountBookmark] { appState.bookmarks(for: hostName) }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            VStack(alignment: .leading, spacing: Spacing.l) {
                addRow
                Divider()
                list
            }
            .padding(Spacing.xl)
            Divider()
            footer
        }
        .frame(width: 520, height: 620)
        .onAppear { appState.reloadMountBookmarks(); pathFocused = true }
    }

    private var header: some View {
        HStack(spacing: Spacing.m) {
            Image(systemName: "pin.fill").font(.title2).foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text("Pinned folders").font(.dashTitle)
                Text(hostName).font(.countBadge).foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, Spacing.xl)
        .padding(.vertical, Spacing.m)
    }

    private var addRow: some View {
        VStack(alignment: .leading, spacing: Spacing.s) {
            Text("Add a folder you work in — mounting it takes you straight there.")
                .font(.rowMeta).foregroundStyle(.secondary)
            HStack(spacing: Spacing.s) {
                TextField("/scratch/you/project", text: $pathDraft)
                    .textFieldStyle(.roundedBorder)
                    .font(.body.monospaced())
                    .focused($pathFocused)
                    .onSubmit(add)
                    .accessibilityLabel("Remote folder path")
                TextField("name (optional)", text: $labelDraft)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 130)
                    .onSubmit(add)
                    .accessibilityLabel("Name for this folder")
                Button("Pin", action: add)
                    .buttonStyle(.glass)
                    .disabled(pathDraft.trimmingCharacters(in: .whitespaces).isEmpty)
                Button {
                    browsing.toggle()
                    if browsing && browseEntries.isEmpty { Task { await loadBrowse(browsePath) } }
                } label: {
                    Label("Browse…", systemImage: "folder.badge.questionmark")
                }
                .buttonStyle(.glass)
                .disabled(!hostIsReady)
                .help(hostIsReady
                      ? "Browse the host's folders instead of typing a path"
                      : "Connect the host first to browse its folders")
            }
            if browsing { browser }
            if let error {
                Text(error).font(.rowMeta).foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var hostIsReady: Bool {
        appState.hosts.first { $0.host == hostName }?.isMasterReady ?? false
    }

    /// Remote folder browser. Runs over the host's existing connection, so it
    /// costs no extra login.
    @ViewBuilder
    private var browser: some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            HStack(spacing: Spacing.xs) {
                Button {
                    Task { await loadBrowse(parentOf(browsePath)) }
                } label: { Image(systemName: "chevron.up") }
                .buttonStyle(.borderless)
                .disabled(browsePath == "/" || browseLoading)
                .help("Up one level")
                .accessibilityLabel("Go up one folder")

                Text(browsePath)
                    .font(.rowMeta.monospaced())
                    .lineLimit(1).truncationMode(.head)
                Spacer()
                if browseLoading { ProgressView().controlSize(.small) }
                Button("Pin this folder") {
                    pathDraft = browsePath
                    add()
                }
                .buttonStyle(.glass)
                .controlSize(.small)
                .disabled(browseLoading)
            }
            if let browseError {
                Text(browseError).font(.rowMeta).foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(browseEntries) { d in
                        Button {
                            Task { await loadBrowse(d.path) }
                        } label: {
                            HStack(spacing: Spacing.xs) {
                                Image(systemName: "folder").foregroundStyle(.tint)
                                Text(d.name).font(.rowMeta)
                                Spacer()
                            }
                            .contentShape(Rectangle())
                            .padding(.vertical, 2)
                            .padding(.horizontal, Spacing.xs)
                        }
                        .buttonStyle(.plain)
                    }
                    if browseEntries.isEmpty && !browseLoading && browseError == nil {
                        Text("No sub-folders here — “Pin this folder” to pin it as-is.")
                            .font(.rowMeta).foregroundStyle(.secondary)
                            .padding(Spacing.xs)
                    }
                }
            }
            .frame(height: 120)
            .groupedContent(cornerRadius: Radius.control)
        }
    }

    private func parentOf(_ path: String) -> String {
        let trimmed = path.hasSuffix("/") && path.count > 1 ? String(path.dropLast()) : path
        guard let idx = trimmed.lastIndex(of: "/"), idx != trimmed.startIndex else { return "/" }
        return String(trimmed[trimmed.startIndex..<idx])
    }

    private func loadBrowse(_ path: String) async {
        browseLoading = true
        browseError = nil
        defer { browseLoading = false }
        do {
            browseEntries = try await appState.listRemoteDirs(host: hostName, path: path)
            browsePath = path
        } catch {
            browseError = (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }

    @ViewBuilder
    private var list: some View {
        if bookmarks.isEmpty {
            VStack(spacing: Spacing.s) {
                Image(systemName: "pin.slash").font(.title2).foregroundStyle(.secondary)
                Text("No pinned folders yet — “Mount” will mount the whole filesystem (/).")
                    .font(.rowMeta).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text("The first one is what plain “Mount” uses. “Auto” mounts it as soon as the host connects.")
                    .font(.rowMeta).foregroundStyle(.secondary)
                List {
                    ForEach(bookmarks) { bm in
                        HStack(spacing: Spacing.s) {
                            Image(systemName: "folder.fill").foregroundStyle(.tint)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(bm.displayName).font(.rowTitle)
                                Text(bm.remotePath)
                                    .font(.rowMeta.monospaced())
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1).truncationMode(.middle)
                            }
                            Spacer()
                            Toggle("Auto", isOn: Binding(
                                get: { bm.autoMount },
                                set: { on in
                                    error = appState.setAutoMount(host: hostName,
                                                                  remotePath: bm.remotePath,
                                                                  autoMount: on)
                                }
                            ))
                            .toggleStyle(.switch)
                            .controlSize(.mini)
                            .help("Mount this folder automatically as soon as the host connects")
                            .accessibilityLabel("Auto-mount \(bm.displayName) on connect")

                            Button {
                                if let host = appState.hosts.first(where: { $0.host == hostName }) {
                                    Task { await appState.toggleMount(host, remotePath: bm.remotePath) }
                                    appState.dismissSheet()
                                }
                            } label: {
                                Label("Mount", systemImage: "externaldrive.badge.plus")
                            }
                            .buttonStyle(.glass)
                            .controlSize(.small)
                            .disabled(!(appState.hosts.first { $0.host == hostName }?.isMasterReady ?? false))

                            Button(role: .destructive) {
                                error = appState.unpinMountFolder(host: hostName,
                                                                  remotePath: bm.remotePath)
                            } label: {
                                Image(systemName: "trash")
                            }
                            .buttonStyle(.borderless)
                            .accessibilityLabel("Unpin \(bm.displayName)")
                        }
                        .listRowBackground(Color.clear)
                    }
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .groupedContent()
            }
        }
    }

    private var footer: some View {
        HStack {
            Toggle("Open in Finder after mounting", isOn: Binding(
                get: { appState.openInFinderAfterMount },
                set: { UserDefaults.standard.set($0, forKey: SettingsKey.openFinderAfterMount) }
            ))
            .toggleStyle(.switch)
            .controlSize(.small)
            Spacer()
            Button("Done") { appState.dismissSheet() }
                .keyboardShortcut(.defaultAction)
        }
        .padding(.horizontal, Spacing.xl)
        .padding(.vertical, Spacing.m)
    }

    private func add() {
        let path = pathDraft
        guard !path.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        error = appState.pinMountFolder(host: hostName, remotePath: path, label: labelDraft)
        if error == nil {
            pathDraft = ""
            labelDraft = ""
            pathFocused = true
        }
    }
}
