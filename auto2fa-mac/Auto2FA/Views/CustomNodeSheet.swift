import SwiftUI

/// Manual-entry escape hatch for when `squeue` doesn't surface the node
/// you want (e.g. a node you reached out-of-band, an interactive shell).
struct CustomNodeSheet: View {
    @EnvironmentObject var appState: AppState
    let tunnelName: String

    @State private var node = ""
    @State private var user = NSUserName()
    @State private var error: String?
    @State private var submitting = false
    @FocusState private var focused: Field?

    enum Field { case node, user }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Custom node for ‘\(tunnelName)’")
                .font(.title2.weight(.semibold))

            VStack(alignment: .leading, spacing: 6) {
                Text("Node").font(.caption).foregroundStyle(.secondary)
                TextField("holygpu8a11103.rc.fas.harvard.edu", text: $node)
                    .textFieldStyle(.roundedBorder)
                    .focused($focused, equals: .node)
                    .onSubmit { focused = .user }
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("User").font(.caption).foregroundStyle(.secondary)
                TextField(NSUserName(), text: $user)
                    .textFieldStyle(.roundedBorder)
                    .focused($focused, equals: .user)
                    .onSubmit { submit() }
            }

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
        .padding(20)
        .frame(width: 460)
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
            await appState.pickNode(for: tunnelName,
                                    node: trimmedNode,
                                    user: trimmedUser.isEmpty ? NSUserName() : trimmedUser)
        }
    }
}
