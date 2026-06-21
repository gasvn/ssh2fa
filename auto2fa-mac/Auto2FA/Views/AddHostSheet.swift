import SwiftUI

/// Two-step wizard for registering a new SSH host with credentials + OTP
/// secret. Step 1: connection info (hostname, user, password, otpauth URL).
/// Step 2: confirmation + auto-connect toggle. Daemon persists to
/// `~/.ssh2fa/passwords.json` (via SSH_CONFIG_PATH env) and spins up a
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
    @State private var displayName = ""        // friendly → sanitized alias
    @State private var serverAddress = ""      // HostName
    @State private var username = ""           // User
    @State private var portText = "22"         // Port (advanced)
    @State private var showAdvanced = false
    @State private var password = ""
    @State private var otpauthURL = ""
    @State private var autoConnect = true
    @State private var showingPassword = false
    @State private var submitting = false
    @State private var testing = false
    @State private var testResult: (ok: Bool, message: String)? = nil
    @State private var error: String?
    @State private var qrError: String?
    @State private var showOTPHelp = false
    /// nil = not checked / empty; false = typed alias isn't a Host in ssh config.
    @State private var hostInConfig: Bool? = nil

    let prefillAlias: String?

    /// Guided ("Add a host") mode collects name/address/username and writes the
    /// app-managed ssh config; the import mode (`prefillAlias != nil`) registers
    /// an alias the user already defined in their own ~/.ssh/config.
    private var isGuided: Bool { prefillAlias == nil }

    init(prefillAlias: String? = nil) {
        self.prefillAlias = prefillAlias
        _hostname = State(initialValue: prefillAlias ?? "")
    }

    /// True iff `alias` is reachable from ~/.ssh/config. Uses the app's cached
    /// parse (`parsedConfig`), which FOLLOWS `Include` directives — so a host
    /// defined in an Include'd file (`config.d/*`) isn't falsely flagged, and
    /// the warning is consistent with the import sheet. Returns true (no warning)
    /// for an empty alias, when the view is incomplete (Match/unresolved
    /// Include), or when a wildcard `Host` pattern covers the alias.
    private func aliasKnown(_ alias: String) -> Bool {
        let a = alias.trimmingCharacters(in: .whitespacesAndNewlines)
        if a.isEmpty { return true }
        let cfg = appState.parsedConfig
        if cfg.incompleteView { return true }
        if cfg.hosts.contains(where: { $0.alias == a }) { return true }
        return cfg.patterns.contains { SSHSyncDiff.globMatches(pattern: $0, name: a) }
    }
    @FocusState private var focused: Field?

    enum Field { case hostname, password, otpauth }

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
            .padding(Spacing.xl)
            Divider()
            footer
        }
        .frame(width: 440)
        .onAppear {
            focused = .hostname
            appState.refreshConfigCache()             // fresh Include-aware view for the warning
            hostInConfig = aliasKnown(hostname)        // evaluate a prefilled alias immediately
        }
    }

    private var header: some View {
        HStack(spacing: Spacing.m) {
            Image(systemName: "server.rack")
                .font(.title2)
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text("Add SSH host")
                    .font(.dashTitle)
                Text("Step \(step + 1) of 2")
                    .font(.countBadge)
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, Spacing.xl)
        .padding(.vertical, Spacing.m)
    }

    private var stepConnection: some View {
        VStack(alignment: .leading, spacing: Spacing.m) {
            // Fields wrapped in a glass card panel
            VStack(alignment: .leading, spacing: Spacing.m) {
                if isGuided {
                    field("Name", TextField("e.g. Cannon, lab server", text: $displayName)
                            .focused($focused, equals: .hostname))
                    field("Server address", TextField("login.rc.fas.harvard.edu", text: $serverAddress))
                    field("Username", TextField("your login name on the server", text: $username))
                    DisclosureGroup("Advanced", isExpanded: $showAdvanced) {
                        field("Port", TextField("22", text: $portText))
                    }
                } else {
                    field("Hostname or SSH alias",
                          VStack(alignment: .leading, spacing: Spacing.xs) {
                            TextField("login01.example.edu", text: $hostname)
                                .focused($focused, equals: .hostname)
                                // NOTE: no username field — the host is an ssh-config alias;
                                // the login user comes from ssh config, and a field here was
                                // never sent anywhere (pure decoration that misled users).
                                .onSubmit { focused = .password }
                                .onChange(of: hostname) { _, _ in hostInConfig = aliasKnown(hostname) }
                            if hostInConfig == false {
                                Label("Not found as a Host in ~/.ssh/config — make sure it's a real ssh alias or a reachable hostname.",
                                      systemImage: "exclamationmark.triangle")
                                    .font(.caption2).foregroundStyle(.orange)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                          })
                }
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
                field("2FA secret (otpauth:// URL or secret key)",
                      VStack(alignment: .leading, spacing: Spacing.xs) {
                        TextField("otpauth://totp/…?secret=…   — or just the secret key",
                                  text: $otpauthURL)
                            .focused($focused, equals: .otpauth)
                        HStack(spacing: Spacing.s) {
                            Button {
                                if let payload = QRDecoder.decodeFromClipboard() {
                                    otpauthURL = payload; qrError = nil
                                } else {
                                    qrError = "No QR on the clipboard — screenshot the QR (⌘⇧⌃4 copies it to the clipboard), then click again."
                                }
                            } label: {
                                Label("Scan QR from clipboard", systemImage: "qrcode.viewfinder")
                            }
                            .controlSize(.small)
                            Spacer()
                        }
                        if let qrError {
                            Text(qrError).font(.caption2).foregroundStyle(.orange)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Text("Paste the full otpauth:// URL or the bare base32 secret — either works. Or screenshot the QR and click “Scan QR”.")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                        DisclosureGroup(isExpanded: $showOTPHelp) {
                            otpHelp
                        } label: {
                            Label("How do I get this?", systemImage: "questionmark.circle")
                                .font(.caption)
                        }
                        .padding(.top, 2)
                      })
            }
            .padding(Spacing.m)
            .groupedContent(cornerRadius: Radius.control)

            // Reassurance line
            HStack(spacing: Spacing.s) {
                Image(systemName: "lock.shield.fill")
                    .foregroundColor(.green)
                Text("Password and OTP secret are stored in your macOS Keychain. Never written to disk in plaintext.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.top, Spacing.xs)

            if let error {
                Text(error).foregroundStyle(.red).font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        // Any credential edit invalidates a previous successful test —
        // without this, Back + edit + Next kept "Add Host" enabled on the
        // STALE "Login succeeded" and saved untested credentials.
        .onChange(of: hostname) { _, _ in invalidateTest() }
        .onChange(of: displayName) { _, _ in invalidateTest() }
        .onChange(of: serverAddress) { _, _ in invalidateTest() }
        .onChange(of: username) { _, _ in invalidateTest() }
        .onChange(of: portText) { _, _ in invalidateTest() }
        .onChange(of: password) { _, _ in invalidateTest() }
        .onChange(of: otpauthURL) { _, _ in invalidateTest() }
    }

    /// In-wizard walkthrough for the thing most newcomers get stuck on:
    /// extracting the TOTP secret (especially from Duo, which hides it behind
    /// "add a new device → manual entry").
    private var otpHelp: some View {
        VStack(alignment: .leading, spacing: Spacing.s) {
            Text("Your 2FA app is seeded by a secret. You need that secret (a base32 string) or the full otpauth:// URL it came from. To reveal it:")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            helpBlock(
                "Duo",
                "Open your Duo self-service / device-management page (often the same prompt you log in with) → Add a new device → choose Tablet or “a different authenticator app” → when it shows the QR, click “Can’t scan it?” / “Manual setup.” Copy the secret key (or the otpauth:// URL) it reveals.")
            helpBlock(
                "Google / GitHub / generic TOTP",
                "When the site shows the authenticator QR code, click “Can’t scan?” / “Enter setup key / manual entry.” Paste the key (or the otpauth:// URL) it shows.")
            helpBlock(
                "Already in an authenticator app?",
                "Most apps can re-export an account’s setup key / otpauth URL from its details screen.")

            Text("Only TOTP codes (the kind you type) are supported — not Duo Push (approve-on-phone). Your secret is stored in the macOS Keychain.")
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 2)
        }
        .padding(.vertical, Spacing.xs)
    }

    private func helpBlock(_ title: String, _ body: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title).font(.caption.weight(.semibold))
            Text(body).font(.caption2).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var stepConfirm: some View {
        VStack(alignment: .leading, spacing: Spacing.m) {
            // Summary card
            VStack(alignment: .leading, spacing: Spacing.s) {
                let summaryHost = isGuided ? SSHConfigManager.sanitizeAlias(displayName) : hostname
                Label {
                    HStack(spacing: 0) {
                        Text("Host: ").foregroundStyle(.secondary)
                        Text(summaryHost).fontDesign(.monospaced)
                    }
                } icon: { Image(systemName: "checkmark.circle.fill").foregroundColor(.green) }

                Label {
                    HStack(spacing: 0) {
                        Text("Password: ").foregroundStyle(.secondary)
                        Text(String(repeating: "•", count: min(password.count, 12)))
                            .fontDesign(.monospaced)
                    }
                } icon: { Image(systemName: "checkmark.circle.fill").foregroundColor(.green) }

                let otpOk = OTPSecret.normalize(input: otpauthURL, account: summaryHost) != nil
                Label {
                    HStack(spacing: 0) {
                        Text("OTP secret: ").foregroundStyle(.secondary)
                        Text(otpOk ? "ready" : "(not a valid secret)")
                            .foregroundColor(otpOk ? .primary : .red)
                    }
                } icon: {
                    Image(systemName: otpOk ? "checkmark.circle.fill" : "xmark.octagon")
                        .foregroundColor(otpOk ? .green : .red)
                }
            }
            .padding(Spacing.m)
            .groupedContent(cornerRadius: Radius.control)

            Divider()

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
            .padding(.vertical, Spacing.xs)

            Toggle("Connect automatically on startup", isOn: $autoConnect)
                .toggleStyle(.checkbox)
            Text("With auto-connect on, the daemon attempts login for this host every time it starts (or every time the app launches it).")
                .font(.caption)
                .foregroundStyle(.secondary)

            if let error {
                Text(error).foregroundStyle(.red).font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, Spacing.xs)
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
        .padding(.horizontal, Spacing.xl)
        .padding(.vertical, Spacing.m)
    }

    @ViewBuilder
    private func field<Content: View>(_ label: String, _ content: Content) -> some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Text(label).font(.caption).foregroundStyle(.secondary)
            content
                .textFieldStyle(.roundedBorder)
        }
    }

    /// Any edit to host/password/otpauth invalidates a previous successful
    /// test — without this, Back + edit + Next kept "Add Host" enabled on the
    /// STALE result and saved untested credentials (auto-retry login storm).
    private func invalidateTest() {
        testResult = nil
    }

    private func advance() {
        if isGuided {
            let name = SSHConfigManager.sanitizeAlias(displayName)
            guard !name.isEmpty else { error = "Give this host a name."; focused = .hostname; return }
            guard !serverAddress.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                error = "Enter the server address."; return
            }
            guard !username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                error = "Enter your username on the server."; return
            }
            // Conflict only against the user's OWN config hosts — exclude the
            // app's managed aliases (once warm-reuse Includes ssh2fa.conf, the
            // parsed config surfaces them, which would falsely block editing a
            // host we created). Editing an existing managed host is an upsert.
            let managed = Set(ManagedHostStore.load(from: appState.managedHostsURL).map { $0.alias })
            let userAliases = appState.parsedConfig.hosts.map { $0.alias }.filter { !managed.contains($0) }
            if SSHConfigManager.aliasConflicts(name, userAliases: userAliases) {
                error = "You already have an SSH host named “\(name)”. Pick a different name."
                focused = .hostname; return
            }
            guard !password.isEmpty else { error = "Password is required."; return }
            guard OTPSecret.normalize(input: otpauthURL, account: name) != nil else {
                error = "Enter a 2FA secret — an otpauth:// URL or a base32 key."
                focused = .otpauth; return
            }
            error = nil
            step = 1
            return
        }
        let h = hostname.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !h.isEmpty else { error = "Hostname is required."; focused = .hostname; return }
        guard !password.isEmpty else { error = "Password is required."; focused = .password; return }
        guard OTPSecret.normalize(input: otpauthURL, account: hostname) != nil else {
            error = "Enter a 2FA secret — an otpauth:// URL or a base32 key."
            focused = .otpauth; return
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
            let ok: Bool, reason: String
            if isGuided {
                // Write the sidecar + app-managed ssh config BEFORE testing so
                // `ssh -F` can resolve the sanitized alias (the guided host isn't
                // in ~/.ssh/config).
                let alias = SSHConfigManager.sanitizeAlias(displayName)
                let port = Int(portText.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 22
                try? ManagedHostStore.upsert(
                    ManagedHostConn(alias: alias,
                                    hostName: serverAddress.trimmingCharacters(in: .whitespacesAndNewlines),
                                    user: username.trimmingCharacters(in: .whitespacesAndNewlines), port: port),
                    in: appState.managedHostsURL)
                appState.syncManagedSSHConfig()
                (ok, reason) = try await appState.client.testHostCredentials(
                    host: alias, password: password,
                    otpauthURL: OTPSecret.normalize(input: otpauthURL, account: alias)
                        ?? otpauthURL.trimmingCharacters(in: .whitespacesAndNewlines))
            } else {
                (ok, reason) = try await appState.client.testHostCredentials(
                    host: hostname.trimmingCharacters(in: .whitespacesAndNewlines),
                    password: password,
                    otpauthURL: OTPSecret.normalize(input: otpauthURL, account: hostname)
                        ?? otpauthURL.trimmingCharacters(in: .whitespacesAndNewlines)
                )
            }
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
        if isGuided {
            let alias = SSHConfigManager.sanitizeAlias(displayName)
            let port = Int(portText.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 22
            Task {
                if let msg = await appState.addManagedHost(
                    alias: alias,
                    hostName: serverAddress.trimmingCharacters(in: .whitespacesAndNewlines),
                    user: username.trimmingCharacters(in: .whitespacesAndNewlines),
                    port: port,
                    password: password,
                    otpauthURL: OTPSecret.normalize(input: otpauthURL, account: alias)
                        ?? otpauthURL.trimmingCharacters(in: .whitespacesAndNewlines),
                    autoConnect: autoConnect
                ) {
                    error = msg; submitting = false
                } else {
                    appState.dismissSheet()
                }
            }
            return
        }
        Task {
            if let msg = await appState.addHost(
                host: hostname.trimmingCharacters(in: .whitespacesAndNewlines),
                password: password,
                otpauthURL: OTPSecret.normalize(input: otpauthURL, account: hostname)
                    ?? otpauthURL.trimmingCharacters(in: .whitespacesAndNewlines),
                autoConnect: autoConnect
            ) {
                error = msg
                submitting = false
            } else if prefillAlias != nil {
                // Launched from the "Add from ~/.ssh/config" importer → go back
                // to the import list (minus the host we just added) so the user
                // can keep enabling more in one sitting.
                appState.presentImport()
            } else {
                appState.dismissSheet()
            }
        }
    }
}
