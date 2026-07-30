import SwiftUI
import AppKit

/// Per-host "Password & setup": see what SSH2FA has stored for one host, and
/// change it.
///
/// Three sections, each answering a question the app previously couldn't:
///  1. **Connection** — where does this alias actually point (HostName / User /
///     Port)? Editable for a guided host (the app owns those values in its
///     sidecar); read-only for an alias the user defined in ~/.ssh/config,
///     which is theirs to edit.
///  2. **Password** — is one stored, and what is it? The value is revealed only
///     after device-owner auth (Touch ID / login password), and is fetched from
///     the daemon on demand — never held in this view unless revealed.
///  3. **Two-factor** — which account the stored TOTP secret belongs to, its
///     live code, and a way to replace it.
///
/// "Test login" verifies the STORED credentials through the daemon (a one-shot
/// login with no master reuse), so checking them never pulls secrets into the UI.
struct HostSettingsSheet: View {
    let hostName: String
    @EnvironmentObject var appState: AppState

    // Loaded metadata (no secrets).
    @State private var creds: BackendClient.HostCredentials?
    @State private var loadError: String?

    // Revealed secrets — only ever set after a successful auth.
    @State private var revealedPassword: String?
    @State private var revealedOTPURL: String?
    @State private var revealing = false

    // Pending edits.
    @State private var editingPassword = false
    @State private var newPassword = ""
    @State private var showNewPassword = false
    @State private var editingOTP = false
    @State private var newOTPInput = ""
    @State private var qrError: String?

    // Connection fields (guided hosts only).
    @State private var editHostName = ""
    @State private var editUser = ""
    @State private var editPort = "22"
    @State private var connSaved = false

    // Status.
    @State private var saving = false
    @State private var testing = false
    @State private var statusMessage: String?
    @State private var statusIsError = false
    @State private var reconnectOffered = false

    @FocusState private var focused: Field?
    private enum Field { case password, otp, hostName, user, port }

    private var host: SSHHost? { appState.hosts.first { $0.host == hostName } }

    /// Where this host's connection settings live. Resolved ONCE on appear (and
    /// after a save) — computing it per render would re-read the sidecar file on
    /// every keystroke.
    @State private var source: HostSettingsCore.ConnectionSource = .unknown

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: Spacing.l) {
                    if let loadError {
                        errorBanner(loadError)
                    }
                    connectionSection
                    passwordSection
                    twoFactorSection
                }
                .padding(Spacing.xl)
            }
            Divider()
            footer
        }
        .frame(width: 460, height: 620)
        .task { await load() }
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Spacing.m) {
            Image(systemName: "key.horizontal.fill")
                .font(.title2)
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text(hostName).font(.dashTitle)
                Text(HostSettingsCore.targetSummary(source) ?? "Password & setup")
                    .font(.countBadge)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if let h = host {
                StatusBadge(host: h.displayState, text: FriendlyText.hostStatus(h.status))
            }
        }
        .padding(.horizontal, Spacing.xl)
        .padding(.vertical, Spacing.m)
    }

    // MARK: - Section 1: connection

    @ViewBuilder
    private var connectionSection: some View {
        card("Connection", systemImage: "network") {
            switch source {
            case .managed:
                VStack(alignment: .leading, spacing: Spacing.m) {
                    labeledField("Server address",
                                 TextField("login.example.edu", text: $editHostName)
                                    .focused($focused, equals: .hostName))
                    labeledField("Username",
                                 TextField("your login name", text: $editUser)
                                    .focused($focused, equals: .user))
                    labeledField("Port",
                                 TextField("22", text: $editPort)
                                    .focused($focused, equals: .port)
                                    .frame(width: 80))
                    HStack(spacing: Spacing.s) {
                        Button("Save connection") { saveConnection() }
                            .buttonStyle(.glass)
                            .disabled(!connectionDirty || connectionValidationError != nil)
                        if let err = connectionValidationError, connectionDirty {
                            Text(err).font(.rowMeta).foregroundStyle(.red)
                        } else if connSaved && !connectionDirty {
                            Label("Saved", systemImage: "checkmark.circle.fill")
                                .font(.rowMeta).foregroundStyle(.green)
                        } else {
                            Text("SSH2FA manages this host's ssh config entry.")
                                .font(.rowMeta).foregroundStyle(.secondary)
                        }
                    }
                }

            case .userConfig(let cfgHostName, let cfgUser):
                VStack(alignment: .leading, spacing: Spacing.s) {
                    readOnlyRow("Server address", cfgHostName ?? "— not set in your config —")
                    readOnlyRow("Username", cfgUser ?? "— not set (ssh uses your Mac username) —")
                    Text("Defined by you in ~/.ssh/config, so SSH2FA doesn't change it. Edit that file to change where “\(hostName)” points.")
                        .font(.rowMeta)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Button {
                        NSWorkspace.shared.selectFile(SSHPaths.configFile(dir: SSHPaths.sshDir()),
                                                      inFileViewerRootedAtPath: SSHPaths.sshDir())
                    } label: {
                        Label("Show ~/.ssh/config in Finder", systemImage: "folder")
                    }
                    .buttonStyle(.glass)
                }

            case .unknown:
                VStack(alignment: .leading, spacing: Spacing.s) {
                    Label("“\(hostName)” isn't a Host in ~/.ssh/config",
                          systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                    Text("It's registered with SSH2FA, but ssh has no entry for this name — so it can't connect. Add a `Host \(hostName)` block to ~/.ssh/config, or re-add it with “Add Host” so SSH2FA manages the entry.")
                        .font(.rowMeta)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private var connectionDirty: Bool {
        guard case .managed(let c) = source else { return false }
        return editHostName != c.hostName || editUser != c.user
            || HostSettingsCore.parsePort(editPort) != c.port
    }

    private var connectionValidationError: String? {
        HostSettingsCore.connectionError(hostName: editHostName, user: editUser, portText: editPort)
    }

    private func saveConnection() {
        guard let port = HostSettingsCore.parsePort(editPort) else { return }
        let err = appState.updateManagedHostConnection(
            alias: hostName,
            hostName: editHostName.trimmingCharacters(in: .whitespacesAndNewlines),
            user: editUser.trimmingCharacters(in: .whitespacesAndNewlines),
            port: port)
        if let err {
            statusMessage = err
            statusIsError = true
        } else {
            connSaved = true
            statusMessage = "Connection settings saved. They apply to the next connection."
            statusIsError = false
            // Pick up the values we just wrote, so "dirty" resets to false.
            reloadSource()
        }
    }

    // MARK: - Section 2: password

    @ViewBuilder
    private var passwordSection: some View {
        card("SSH password", systemImage: "lock.fill") {
            VStack(alignment: .leading, spacing: Spacing.m) {
                if creds == nil && loadError == nil {
                    // Don't render a fake mask before we know what's stored.
                    HStack(spacing: Spacing.s) {
                        ProgressView().controlSize(.small)
                        Text("Checking what's stored…").font(.rowMeta).foregroundStyle(.secondary)
                    }
                } else if let creds, !creds.has_password {
                    Label("No password stored for this host", systemImage: "exclamationmark.circle")
                        .font(.rowMeta)
                        .foregroundStyle(.orange)
                } else {
                    HStack(spacing: Spacing.s) {
                        if let revealedPassword {
                            Text(revealedPassword)
                                .font(RowMetric.mono)
                                .textSelection(.enabled)
                                .lineLimit(1)
                                .truncationMode(.tail)
                        } else {
                            Text(HostSettingsCore.passwordMask(length: creds?.password_length ?? 8))
                                .font(RowMetric.mono)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if revealing {
                            ProgressView().controlSize(.small)
                        } else if revealedPassword == nil {
                            Button("Reveal") { Task { await reveal() } }
                                .buttonStyle(.glass)
                                .help("Asks for Touch ID (or your Mac password) first")
                        } else {
                            Button {
                                copyToClipboard(revealedPassword ?? "")
                            } label: {
                                Label("Copy", systemImage: "doc.on.doc")
                            }
                            .buttonStyle(.glass)
                            Button("Hide") { revealedPassword = nil; revealedOTPURL = nil }
                                .buttonStyle(.glass)
                        }
                    }
                }

                Divider()

                DisclosureGroup("Change password", isExpanded: $editingPassword) {
                    VStack(alignment: .leading, spacing: Spacing.s) {
                        HStack(spacing: Spacing.xs) {
                            if showNewPassword {
                                TextField("new password", text: $newPassword)
                                    .focused($focused, equals: .password)
                            } else {
                                SecureField("new password", text: $newPassword)
                                    .focused($focused, equals: .password)
                            }
                            Button {
                                showNewPassword.toggle()
                            } label: {
                                Image(systemName: showNewPassword ? "eye.slash" : "eye")
                            }
                            .buttonStyle(.borderless)
                            .help(showNewPassword ? "Hide" : "Show what you typed")
                        }
                        .textFieldStyle(.roundedBorder)
                        Text("Use this after changing your password on the server. SSH2FA can't detect that change on its own — logins keep failing until the stored password matches.")
                            .font(.rowMeta)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.top, Spacing.s)
                }
                .font(.rowMeta)
            }
        }
    }

    // MARK: - Section 3: two-factor

    @ViewBuilder
    private var twoFactorSection: some View {
        card("Two-factor secret", systemImage: "clock.badge.checkmark") {
            VStack(alignment: .leading, spacing: Spacing.m) {
                if let creds {
                    if let otpError = creds.otp_error {
                        VStack(alignment: .leading, spacing: Spacing.xs) {
                            Label("The stored 2FA secret can't be read",
                                  systemImage: "exclamationmark.triangle.fill")
                                .foregroundStyle(.red)
                            Text(otpError).font(.rowMeta).foregroundStyle(.secondary)
                            Text("Replace it below — logins will keep failing until you do.")
                                .font(.rowMeta).foregroundStyle(.secondary)
                        }
                    } else if creds.has_otp_secret {
                        HStack(spacing: Spacing.s) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(HostSettingsCore.otpSummary(
                                        issuer: creds.otp.issuer,
                                        account: creds.otp.account,
                                        algorithm: creds.otp.algorithm,
                                        digits: creds.otp.digits,
                                        period: creds.otp.period))
                                    .font(.rowIdentifier)
                                Text("Current code")
                                    .font(.rowMeta).foregroundStyle(.secondary)
                            }
                            Spacer()
                            TOTPCodeChip(host: hostName)
                        }
                        if let revealedOTPURL, !revealedOTPURL.isEmpty {
                            HStack(spacing: Spacing.s) {
                                Text(revealedOTPURL)
                                    .font(RowMetric.mono)
                                    .textSelection(.enabled)
                                    .lineLimit(2)
                                    .truncationMode(.middle)
                                Spacer()
                                Button {
                                    copyToClipboard(revealedOTPURL)
                                } label: {
                                    Label("Copy", systemImage: "doc.on.doc")
                                }
                                .buttonStyle(.glass)
                            }
                        } else if revealing {
                            ProgressView().controlSize(.small)
                        } else {
                            Button("Reveal setup URL") { Task { await reveal() } }
                                .buttonStyle(.glass)
                                .help("Asks for Touch ID (or your Mac password) first. Use it to set up the same account in another authenticator.")
                        }
                    } else {
                        Label("No 2FA secret stored — this host can't complete a login prompt",
                              systemImage: "exclamationmark.circle")
                            .font(.rowMeta)
                            .foregroundStyle(.orange)
                    }
                }

                Divider()

                DisclosureGroup(creds?.has_otp_secret == true ? "Replace 2FA secret" : "Add 2FA secret",
                                isExpanded: $editingOTP) {
                    VStack(alignment: .leading, spacing: Spacing.s) {
                        TextField("otpauth://totp/…?secret=…   — or just the secret key",
                                  text: $newOTPInput)
                            .textFieldStyle(.roundedBorder)
                            .focused($focused, equals: .otp)
                        HStack(spacing: Spacing.s) {
                            Button {
                                if let payload = QRDecoder.decodeFromClipboard() {
                                    newOTPInput = payload; qrError = nil
                                } else {
                                    qrError = "No QR on the clipboard — screenshot the QR (⌘⇧⌃4 copies it), then click again."
                                }
                            } label: {
                                Label("Scan QR from clipboard", systemImage: "qrcode.viewfinder")
                            }
                            .buttonStyle(.glass)
                        }
                        if let qrError {
                            Text(qrError).font(.rowMeta).foregroundStyle(.orange)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Text("Needs a TOTP secret key — not Duo Push. Re-enrol the account in your 2FA provider's device page to get a fresh key.")
                            .font(.rowMeta)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.top, Spacing.s)
                }
                .font(.rowMeta)
            }
        }
    }

    // MARK: - Footer

    private var footer: some View {
        VStack(alignment: .leading, spacing: Spacing.s) {
            if let statusMessage {
                HStack(spacing: Spacing.xs) {
                    Image(systemName: statusIsError ? "exclamationmark.triangle.fill"
                                                    : "checkmark.circle.fill")
                        .foregroundStyle(statusIsError ? .red : .green)
                    Text(statusMessage)
                        .font(.rowMeta)
                        .foregroundStyle(statusIsError ? .red : .secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer()
                    if reconnectOffered {
                        Button("Reconnect now") {
                            reconnectOffered = false
                            if let h = host { Task { await appState.retryHost(h) } }
                        }
                        .buttonStyle(.glass)
                    }
                }
            }
            HStack(spacing: Spacing.s) {
                Button {
                    Task { await testLogin() }
                } label: {
                    if testing {
                        HStack(spacing: Spacing.xs) {
                            ProgressView().controlSize(.small)
                            Text("Testing…")
                        }
                    } else {
                        Label("Test login", systemImage: "checkmark.shield")
                    }
                }
                .buttonStyle(.glass)
                .disabled(testing || saving || hasPendingSecretEdits)
                .help(hasPendingSecretEdits
                      ? "Save your changes first — the test uses the stored credentials."
                      : "Try a one-shot login with the stored credentials (no effect on your live connection)")

                Spacer()

                Button("Close") { appState.dismissSheet() }
                    .keyboardShortcut(.cancelAction)

                Button {
                    Task { await saveSecrets() }
                } label: {
                    if saving {
                        HStack(spacing: Spacing.xs) {
                            ProgressView().controlSize(.small)
                            Text("Saving…")
                        }
                    } else {
                        Text("Save changes")
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(saving || !hasPendingSecretEdits)
            }
        }
        .padding(.horizontal, Spacing.xl)
        .padding(.vertical, Spacing.m)
    }

    private var hasPendingSecretEdits: Bool {
        let c = HostSettingsCore.pendingChanges(newPassword: newPassword,
                                                newOTPInput: newOTPInput,
                                                alias: hostName)
        return c.password != nil || c.otpauthURL != nil
    }

    // MARK: - Actions

    private func load() async {
        // Resolve the connection source first so the header/sections have it even
        // if the daemon call below is slow or fails.
        reloadSource()
        do {
            let c = try await appState.hostCredentials(hostName)
            creds = c
            loadError = nil
        } catch {
            loadError = FriendlyText.credentialError(
                (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription)
        }
    }

    /// Re-read where the connection settings live, and seed the edit fields from
    /// them when the app owns the host.
    private func reloadSource() {
        source = appState.connectionSource(for: hostName)
        if case .managed(let c) = source {
            editHostName = c.hostName
            editUser = c.user
            editPort = String(c.port)
        }
    }

    /// Reveal BOTH secrets behind one authentication — asking twice for the same
    /// window (password, then 2FA URL) is pure friction.
    private func reveal() async {
        guard await BiometricLock.confirm(
                reason: "Reveal the stored credentials for \(hostName)") else {
            statusMessage = "Authentication cancelled — nothing was revealed."
            statusIsError = true
            return
        }
        revealing = true
        defer { revealing = false }
        do {
            let (pw, url) = try await appState.revealHostCredentials(hostName)
            revealedPassword = pw
            revealedOTPURL = url
            statusMessage = nil
        } catch {
            statusMessage = FriendlyText.credentialError(
                (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription)
            statusIsError = true
        }
    }

    private func saveSecrets() async {
        let pending = HostSettingsCore.pendingChanges(newPassword: newPassword,
                                                      newOTPInput: newOTPInput,
                                                      alias: hostName)
        guard pending.password != nil || pending.otpauthURL != nil else { return }
        saving = true
        defer { saving = false }
        let outcome = await appState.updateHostCredentials(host: hostName,
                                                          password: pending.password,
                                                          otpauthURL: pending.otpauthURL)
        switch outcome {
        case .saved(let reconnectRequired):
            // Clear the drafts + any stale revealed values so the view can't show
            // the old secret next to the new one.
            newPassword = ""
            newOTPInput = ""
            editingPassword = false
            editingOTP = false
            revealedPassword = nil
            revealedOTPURL = nil
            statusIsError = false
            reconnectOffered = reconnectRequired
            statusMessage = reconnectRequired
                ? "Saved. “\(hostName)” is still on its existing connection — reconnect to use the new credentials."
                : "Saved. They'll be used the next time SSH2FA connects."
            // Refresh the metadata (length / issuer / account all just changed).
            if let c = try? await appState.hostCredentials(hostName) { creds = c }
        case .failed(let message):
            statusMessage = FriendlyText.credentialError(message)
            statusIsError = true
            reconnectOffered = false
        }
    }

    private func testLogin() async {
        testing = true
        defer { testing = false }
        let (ok, reason) = await appState.testStoredCredentials(host: hostName)
        statusIsError = !ok
        reconnectOffered = false
        statusMessage = ok
            ? "Login succeeded with the stored credentials."
            : (reason.isEmpty ? "Login failed." : "Login failed: \(FriendlyText.credentialError(reason))")
    }

    private func copyToClipboard(_ s: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
        statusIsError = false
        statusMessage = "Copied to the clipboard."
    }

    // MARK: - Small building blocks

    @ViewBuilder
    private func card<Content: View>(_ title: String, systemImage: String,
                                     @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: Spacing.m) {
            Label(title, systemImage: systemImage)
                .font(.rowTitle)
                .foregroundStyle(.secondary)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Spacing.l)
        .glassCard(cornerRadius: Radius.control)
    }

    @ViewBuilder
    private func labeledField<F: View>(_ label: String, _ field: F) -> some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Text(label).font(.rowMeta).foregroundStyle(.secondary)
            field.textFieldStyle(.roundedBorder)
        }
    }

    @ViewBuilder
    private func readOnlyRow(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Spacing.s) {
            Text(label)
                .font(.rowMeta)
                .foregroundStyle(.secondary)
                .frame(width: 110, alignment: .leading)
            Text(value)
                .font(.rowIdentifier)
                .textSelection(.enabled)
        }
    }

    @ViewBuilder
    private func errorBanner(_ message: String) -> some View {
        HStack(spacing: Spacing.s) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
            Text(message).font(.rowMeta).fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Spacing.m)
        .glassCard(cornerRadius: Radius.control)
    }
}
