import SwiftUI

/// Lists running SLURM jobs from the daemon's `discover_nodes`. The user picks
/// one (or hits "Custom node" for manual entry, which opens CustomNodeSheet).
struct NodePickerSheet: View {
    @EnvironmentObject var appState: AppState
    let tunnelName: String

    @State private var jobs: [SqueueJob] = []
    @State private var loading = true
    @State private var submitting = false
    @State private var error: String?
    @State private var selection: SqueueJob.ID?
    @State private var loadedJumpName: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("Pick a compute node for ‘\(tunnelName)’")
                    .font(.title2.weight(.semibold))
                if let jump = loadedJumpName {
                    Text("via \(jump)")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
                Spacer()
            }

            if loading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Loading jobs from squeue…")
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 20)
            } else if let error {
                VStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle")
                        .foregroundStyle(.orange)
                    Text(error)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 20)
            } else if jobs.isEmpty {
                Text("No running jobs found. Use a custom node, or start a SLURM job first.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 20)
            } else {
                Table(jobs, selection: $selection) {
                    TableColumn("JobID") { Text($0.jobid).fontDesign(.monospaced) }
                        .width(min: 70)
                    TableColumn("Partition") { Text($0.partition) }
                        .width(min: 80, ideal: 110)
                    TableColumn("Name") { Text($0.name).lineLimit(1) }
                        .width(min: 80, ideal: 120)
                    TableColumn("Time") { Text($0.time).fontDesign(.monospaced) }
                        .width(min: 80, ideal: 110)
                    TableColumn("Node") { Text($0.node).fontDesign(.monospaced) }
                }
                .frame(minHeight: 200)
            }

            HStack {
                Button {
                    Task { await load() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(loading || submitting)

                Button {
                    appState.presentCustomNode(for: tunnelName)
                } label: {
                    Label("Custom node…", systemImage: "keyboard")
                }
                .disabled(submitting)

                Spacer()

                Button("Cancel") { appState.dismissSheet() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(submitting)
                Button {
                    pick()
                } label: {
                    if submitting { ProgressView().controlSize(.small) }
                    else { Text("Use selected") }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(selection == nil || submitting)
            }
        }
        .padding(Spacing.l)
        .frame(width: 720)
        .task { await load() }
    }

    private func pickActiveJump(for tunnel: Tunnel) -> String? {
        let candidates = tunnel.jumpCandidates ?? appState.hosts.map { $0.host }
        for name in candidates {
            if let h = appState.hosts.first(where: { $0.host == name }), h.isMasterReady {
                return name
            }
        }
        return nil
    }

    private func load() async {
        guard let tunnel = appState.tunnels.first(where: { $0.name == tunnelName }) else {
            loading = false
            error = "Tunnel not found."
            return
        }
        guard let jump = pickActiveJump(for: tunnel) else {
            loading = false
            error = "No connected jump host. Start one of the hosts in the top panel first."
            return
        }
        loadedJumpName = jump
        loading = true
        error = nil
        do {
            let fetched = try await appState.client.discoverNodes(host: jump)
            jobs = fetched
            // Preselect previously-used node if it's still running
            if let last = tunnel.lastNode,
               let prev = fetched.first(where: { $0.node == last }) {
                selection = prev.id
            } else if let first = fetched.first {
                selection = first.id
            }
            loading = false
        } catch {
            self.error = (error as? BackendClient.ClientError)?.errorDescription
                       ?? error.localizedDescription
            loading = false
        }
    }

    private func pick() {
        guard !submitting else { return }
        guard let id = selection, let job = jobs.first(where: { $0.id == id }) else { return }
        let user = appState.tunnels.first(where: { $0.name == tunnelName })?.lastUser ?? NSUserName()
        submitting = true
        error = nil
        Task {
            if let errMsg = await appState.pickNode(
                for: tunnelName, node: job.node, user: user
            ) {
                error = errMsg  // surface in the picker; don't dismiss
                submitting = false
            }
            // on success: appState.pickNode dismisses the sheet, no need to clear submitting
        }
    }
}
