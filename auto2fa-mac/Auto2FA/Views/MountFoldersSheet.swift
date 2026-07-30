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
        .frame(width: 480, height: 460)
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
            }
            if let error {
                Text(error).font(.rowMeta).foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
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
                Text("The first one is what plain “Mount” uses.")
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
