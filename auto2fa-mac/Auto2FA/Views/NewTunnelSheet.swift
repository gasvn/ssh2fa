import SwiftUI

/// Create-tunnel form. Name + local port. Submit / Cancel.
/// Validates inline; daemon returns DUPLICATE / PORT_IN_USE / BAD_PARAMS errors
/// which surface in the `error` Label below the form.
struct NewTunnelSheet: View {
    @EnvironmentObject var appState: AppState
    @State private var name = ""
    @State private var portText = "8888"
    @State private var error: String?
    @State private var submitting = false
    @FocusState private var focused: Field?

    enum Field { case name, port }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("New Tunnel")
                .font(.title2.weight(.semibold))

            VStack(alignment: .leading, spacing: 6) {
                Text("Name").font(.caption).foregroundStyle(.secondary)
                TextField("jupyter", text: $name)
                    .textFieldStyle(.roundedBorder)
                    .focused($focused, equals: .name)
                    .onSubmit { focused = .port }
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Local port").font(.caption).foregroundStyle(.secondary)
                TextField("8888", text: $portText)
                    .textFieldStyle(.roundedBorder)
                    .focused($focused, equals: .port)
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
                    else { Text("Submit") }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(submitting)
            }
        }
        .padding(20)
        .frame(width: 360)
        .onAppear { focused = .name }
    }

    private func submit() {
        guard !submitting else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            error = "Name cannot be empty."
            focused = .name
            return
        }
        guard let port = Int(portText.trimmingCharacters(in: .whitespacesAndNewlines)),
              port >= 1024, port <= 65535 else {
            error = "Local port must be 1024–65535."
            focused = .port
            return
        }
        submitting = true
        error = nil
        Task {
            if let errMsg = await appState.createTunnel(name: trimmedName, localPort: port) {
                error = errMsg
                submitting = false
            }
        }
    }
}
