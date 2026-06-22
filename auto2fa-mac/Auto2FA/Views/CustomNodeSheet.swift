import SwiftUI

/// Manual-entry escape hatch for when `squeue` doesn't surface the node
/// you want (e.g. a node you reached out-of-band, an interactive shell).
struct CustomNodeSheet: View {
    @EnvironmentObject var appState: AppState
    let tunnelName: String

    @State private var node = ""
    @State private var user = ""  // empty → daemon keeps existing / remote $USER
    @State private var error: String?
    @State private var submitting = false
    @FocusState private var focused: Field?

    enum Field { case node, user }

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.l) {
            Text("Custom node for '\(tunnelName)'")
                .font(.dashTitle)

            // Fields wrapped in a glass card panel
            VStack(alignment: .leading, spacing: Spacing.m) {
                VStack(alignment: .leading, spacing: Spacing.xs) {
                    Text("Node").font(.caption).foregroundStyle(.secondary)
                    TextField("gpunode8a11103.hpc.example.edu", text: $node)
                        .textFieldStyle(.roundedBorder)
                        .focused($focused, equals: .node)
                        .onSubmit { focused = .user }
                }
                VStack(alignment: .leading, spacing: Spacing.xs) {
                    Text("User").font(.caption).foregroundStyle(.secondary)
                    TextField("cluster username (optional)", text: $user)
                        .textFieldStyle(.roundedBorder)
                        .focused($focused, equals: .user)
                        .onSubmit { submit() }
                }
            }
            .padding(Spacing.m)
            .groupedContent(cornerRadius: Radius.control)

            if let error {
                Text(error)
                    .foregroundStyle(.red)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Cancel") { appState.dismissSheet() }
                    .keyboardShortcut(.cancelAction)
                Button {
                    submit()
                } label: {
                    if submitting { ProgressView().controlSize(.small) }
                    else { Text("Connect") }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(submitting)
            }
        }
        .padding(Spacing.xl)
        .frame(width: 440)
        .onAppear { focused = .node }
    }

    private func submit() {
        guard !submitting else { return }
        let trimmedNode = node
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "\n", with: "")
            .replacingOccurrences(of: "\r", with: "")
        guard !trimmedNode.isEmpty else {
            error = "Node cannot be empty."
            focused = .node
            return
        }
        let trimmedUser = user
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "\n", with: "")
        submitting = true
        error = nil
        Task {
            if let errMsg = await appState.pickNode(
                for: tunnelName,
                node: trimmedNode,
                user: trimmedUser  // empty → daemon keeps existing last_user
            ) {
                error = errMsg
                submitting = false
            }
        }
    }
}
