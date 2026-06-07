import SwiftUI

/// Two-step wizard for registering a new SSH host with credentials + OTP
/// secret. Step 1: connection info (hostname, user, password, otpauth URL).
/// Step 2: confirmation + auto-connect toggle. Daemon persists to
/// `~/.auto2fa/passwords.json` (via SSH_CONFIG_PATH env) and spins up a
/// manager immediately.
///
/// The auth flow we ask the user to feed in:
///   - Password — their SSH password
///   - otpauth URL — pasted from a "Show secret" / "Add account" QR
///     readout. We extract the secret via the same regex backend.py uses.
struct AddHostSheet: View {
    @EnvironmentObject var appState: AppState

    @State private var step = 0
    @State private var hostname = ""
    @State private var user = NSUserName()
    @State private var password = ""
    @State private var otpauthURL = ""
    @State private var autoConnect = true
    @State private var showingPassword = false
    @State private var submitting = false
    @State private var testing = false
    @State private var testResult: (ok: Bool, message: String)? = nil
    @State private var error: String?
    @FocusState private var focused: Field?

    enum Field { case hostname, user, password, otpauth }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            Group {
                if step == 0 {
                    stepConnection
                } else {
                    stepConfirm
                }
            }
            .padding(20)
            Divider()
            footer
        }
        .frame(width: 440)
        .onAppear { focused = .hostname }
    }

    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "server.rack")
                .font(.title2)
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text("Add SSH host")
                    .font(.headline)
                Text("Step \(step + 1) of 2")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
        .background(.bar)
    }

    private var stepConnection: some View {
        VStack(alignment: .leading, spacing: 14) {
            field("Hostname or SSH alias",
                  TextField("login01.example.edu", text: $hostname)
                    .focused($focused, equals: .hostname)
                    .onSubmit { focused = .user })
            field("SSH username",
                  TextField(NSUserName(), text: $user)
                    .focused($focused, equals: .user)
                    .onSubmit { focused = .password })
            field("Password",
                  HStack {
                    Group {
                        if showingPassword {
                            TextField("password", text: $password)
                        } else {
                            SecureField("password", text: $password)
                        }
                    }
                    .focused($focused, equals: .password)
                    .onSubmit { focused = .otpauth }
                    Button {
                        showingPassword.toggle()
                    } label: {
                        Image(systemName: showingPassword ? "eye.slash" : "eye")
                    }
                    .buttonStyle(.borderless)
                    .help(showingPassword ? "Hide" : "Show")
                  })
            field("OTP secret (otpauth:// URL)",
                  VStack(alignment: .leading, spacing: 4) {
                    TextField("otpauth://totp/SiteName:user?secret=...",
                              text: $otpauthURL)
                        .focused($focused, equals: .otpauth)
                    Text("Paste the full URL from your 2FA setup page. We extract the secret automatically.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                  })

            // Reassurance line — users care about where the password ends up.
            HStack(spacing: 6) {
                Image(systemName: "lock.shield.fill")
                    .foregroundColor(.green)
                Text("Password and OTP secret are stored in your macOS Keychain. Never written to disk in plaintext.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.top, 6)
            if let error {
                Text(error).foregroundStyle(.red).font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var stepConfirm: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label {
                HStack(spacing: 0) {
                    Text("Host: ").foregroundStyle(.secondary)
                    Text("\(user)@\(hostname)").fontDesign(.monospaced)
                }
            } icon: { Image(systemName: "checkmark.circle.fill").foregroundColor(.green) }

            Label {
                HStack(spacing: 0) {
                    Text("Password: ").foregroundStyle(.secondary)
                    Text(String(repeating: "•", count: min(password.count, 12)))
                        .fontDesign(.monospaced)
                }
            } icon: { Image(systemName: "checkmark.circle.fill").foregroundColor(.green) }

            let otpOk = otpauthURL.lowercased().contains("secret=")
            Label {
                HStack(spacing: 0) {
                    Text("OTP secret: ").foregroundStyle(.secondary)
                    Text(otpOk ? "extracted" : "(missing secret= param)")
                        .foregroundColor(otpOk ? .primary : .red)
                }
            } icon: {
                Image(systemName: otpOk ? "checkmark.circle.fill" : "xmark.octagon")
                    .foregroundColor(otpOk ? .green : .red)
            }

            Divider().padding(.vertical, 4)

            // Test-login block — refuses to enable Add Host until we've
            // confirmed creds work. This prevents the "17 failed logins
            // rate-limit" cascade that happened when we wrote bad creds
            // and a manager started retrying.
            HStack {
                if testing {
                    ProgressView().controlSize(.small).scaleEffect(0.7)
                    Text("Testing login…").foregroundStyle(.secondary)
                } else if let r = testResult {
                    Image(systemName: r.ok ? "checkmark.circle.fill" : "xmark.octagon")
                        .foregroundColor(r.ok ? .green : .red)
                    Text(r.message)
                        .foregroundColor(r.ok ? .primary : .red)
                        .fixedSize(horizontal: false, vertical: true)
                } else {
                    Image(systemName: "questionmark.circle").foregroundStyle(.secondary)
                    Text("Click \"Test login\" to verify before saving.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    Task { await testLogin() }
                } label: {
                    Text(testing ? "Testing…" : "Test login")
                }
                .disabled(testing)
            }
            .padding(.vertical, 4)

            Toggle("Connect automatically on startup", isOn: $autoConnect)
                .toggleStyle(.checkbox)
            Text("With auto-connect on, the daemon attempts login for this host every time it starts (or every time the app launches it).")
                .font(.caption)
                .foregroundStyle(.secondary)

            if let error {
                Text(error).foregroundStyle(.red).font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, 6)
            }
        }
    }

    private var footer: some View {
        HStack {
            Button("Cancel") { appState.dismissSheet() }
                .keyboardShortcut(.cancelAction)
                .disabled(submitting)
            Spacer()
            if step > 0 {
                Button("Back") { step -= 1; error = nil }
                    .disabled(submitting)
            }
            Button(step == 0 ? "Next" : "Add Host") {
                if step == 0 { advance() } else { submit() }
            }
            .keyboardShortcut(.defaultAction)
            // On step 1, only allow Add Host after a successful test login.
            .disabled(submitting || (step == 1 && (testResult?.ok != true)))
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .background(.bar)
    }

    @ViewBuilder
    private func field<Content: View>(_ label: String, _ content: Content) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label).font(.caption).foregroundStyle(.secondary)
            content
                .textFieldStyle(.roundedBorder)
        }
    }

    private func advance() {
        let h = hostname.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !h.isEmpty else { error = "Hostname is required."; focused = .hostname; return }
        guard !password.isEmpty else { error = "Password is required."; focused = .password; return }
        guard otpauthURL.lowercased().contains("secret=") else {
            error = "OTP URL must contain a `secret=` parameter."; focused = .otpauth; return
        }
        error = nil
        step = 1
    }

    private func testLogin() async {
        guard !testing else { return }
        testing = true
        testResult = nil
        error = nil
        do {
            let (ok, reason) = try await appState.client.testHostCredentials(
                host: hostname.trimmingCharacters(in: .whitespacesAndNewlines),
                password: password,
                otpauthURL: otpauthURL.trimmingCharacters(in: .whitespacesAndNewlines)
            )
            testResult = (ok, ok ? "Login succeeded — you can save now." : reason)
        } catch {
            testResult = (false, "Test couldn't run: \(error.localizedDescription)")
        }
        testing = false
    }

    private func submit() {
        guard !submitting else { return }
        submitting = true
        error = nil
        Task {
            if let msg = await appState.addHost(
                host: hostname.trimmingCharacters(in: .whitespacesAndNewlines),
                password: password,
                otpauthURL: otpauthURL.trimmingCharacters(in: .whitespacesAndNewlines),
                autoConnect: autoConnect
            ) {
                error = msg
                submitting = false
            } else {
                appState.dismissSheet()
            }
        }
    }
}
