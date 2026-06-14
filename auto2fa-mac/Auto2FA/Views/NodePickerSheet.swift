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
    @State private var filter: String = ""

    /// Jobs passing the filter, RUNNING (state R) first, then by JobID.
    private var visibleJobs: [SqueueJob] {
        jobs
            .filter { SearchFilter.matches(query: filter,
                                           in: [$0.jobid, $0.partition, $0.name, $0.node, $0.state]) }
            .sorted { a, b in
                let ar = a.state.uppercased().hasPrefix("R")
                let br = b.state.uppercased().hasPrefix("R")
                if ar != br { return ar }
                return a.jobid < b.jobid
            }
    }

    /// Recently-used nodes that are still running in the loaded list — one-click
    /// re-select.
    private var recentRunningNodes: [String] {
        let live = Set(jobs.map { $0.node })
        return RecentNodes.all().filter { live.contains($0) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.l) {
            // Title row
            HStack(alignment: .firstTextBaseline, spacing: Spacing.s) {
                Text("Pick a compute node for '\(tunnelName)'")
                    .font(.dashTitle)
                if let jump = loadedJumpName {
                    Text("via \(jump)")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
                Spacer()
            }

            // Recent nodes (still running) — one-click re-select.
            if !recentRunningNodes.isEmpty {
                HStack(spacing: Spacing.xs) {
                    Text("Recent:").font(.caption).foregroundStyle(.secondary)
                    ForEach(recentRunningNodes, id: \.self) { node in
                        Button {
                            if let job = jobs.first(where: { $0.node == node }) { selection = job.id }
                        } label: {
                            Text(node).font(.caption.monospaced())
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .help("Select the job on \(node)")
                    }
                    Spacer()
                }
            }

            // Filter — squeue can return dozens of jobs.
            if !loading && error == nil && !jobs.isEmpty {
                HStack(spacing: Spacing.s) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("Filter by job, partition, name, node…", text: $filter)
                        .textFieldStyle(.roundedBorder)
                    if !filter.isEmpty {
                        Button { filter = "" } label: { Image(systemName: "xmark.circle.fill") }
                            .buttonStyle(.borderless)
                    }
                }
            }

            // Table / state container — opaque grouped surface inside the
            // sheet's own system glass (avoid glass-on-glass).
            Group {
                if loading {
                    HStack(spacing: Spacing.s) {
                        ProgressView().controlSize(.small)
                        Text("Loading jobs from squeue…")
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, Spacing.xl)
                } else if let error {
                    VStack(spacing: Spacing.s) {
                        Image(systemName: "exclamationmark.triangle")
                            .foregroundStyle(.orange)
                        Text(error)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, Spacing.xl)
                } else if jobs.isEmpty {
                    Text("No running jobs found. Use a custom node, or start a SLURM job first.")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.vertical, Spacing.xl)
                } else if visibleJobs.isEmpty {
                    Text("No jobs match “\(filter)”.")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.vertical, Spacing.xl)
                } else {
                    Table(visibleJobs, selection: $selection) {
                        TableColumn("State") { job in
                            Text(job.state)
                                .font(.caption.monospaced())
                                .foregroundStyle(job.state.uppercased().hasPrefix("R") ? Color.green : Color.secondary)
                        }
                        .width(min: 44, ideal: 54)
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
            }
            .groupedContent(cornerRadius: Radius.control)

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
        .padding(Spacing.xl)
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
        // Empty user = "daemon keeps the existing last_user / falls back to
        // $USER". NSUserName() here poisoned last_user with the MAC login
        // name — if it differs from the cluster account, the squeue
        // staleness check queries the wrong user and kills a working tunnel.
        let user = appState.tunnels.first(where: { $0.name == tunnelName })?.lastUser ?? ""
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
