import Foundation

/// Pure decision logic behind the per-host "Password & setup" sheet.
///
/// Foundation-only so it unit-tests headlessly: the sheet itself only renders
/// what these functions decide.
enum HostSettingsCore {

    // MARK: - Where a host's connection settings live

    /// Who owns the `HostName` / `User` / `Port` for an alias.
    enum ConnectionSource: Equatable {
        /// SSH2FA wrote this host (guided add) — the app owns the values in its
        /// sidecar and can edit them.
        case managed(ManagedHostConn)
        /// The user defined the alias in their own ~/.ssh/config. We show what we
        /// parsed but must not rewrite their file.
        case userConfig(hostName: String?, user: String?)
        /// Registered with the daemon, but no sidecar entry AND not found in the
        /// config — the alias can't resolve, so it won't connect.
        case unknown
    }

    /// Decide where `alias`'s connection settings come from.
    ///
    /// The sidecar wins: for a guided host the managed conf is included ahead of
    /// the user's config in the daemon's `ssh -F` wrapper, so its values are the
    /// ones that actually apply.
    static func connectionSource(alias: String,
                                 sidecar: [ManagedHostConn],
                                 configHosts: [ConfigHost]) -> ConnectionSource {
        if let conn = sidecar.first(where: { $0.alias == alias }) {
            return .managed(conn)
        }
        // ssh `Host` matching is case-insensitive.
        if let c = configHosts.first(where: { $0.alias.lowercased() == alias.lowercased() }) {
            return .userConfig(hostName: c.hostName, user: c.user)
        }
        return .unknown
    }

    /// True iff the sheet may edit the connection fields for this source.
    static func isEditable(_ source: ConnectionSource) -> Bool {
        if case .managed = source { return true }
        return false
    }

    /// One-line summary of the effective ssh target, e.g. `alice@login.example.edu:2222`.
    /// Returns nil when neither a host name nor a user is known.
    static func targetSummary(_ source: ConnectionSource) -> String? {
        switch source {
        case .managed(let c):
            let base = c.user.isEmpty ? c.hostName : "\(c.user)@\(c.hostName)"
            if base.isEmpty { return nil }
            return c.port == 22 ? base : "\(base):\(c.port)"
        case .userConfig(let hostName, let user):
            let h = hostName?.trimmingCharacters(in: .whitespaces) ?? ""
            let u = user?.trimmingCharacters(in: .whitespaces) ?? ""
            if h.isEmpty && u.isEmpty { return nil }
            if u.isEmpty { return h }
            if h.isEmpty { return "\(u)@?" }
            return "\(u)@\(h)"
        case .unknown:
            return nil
        }
    }

    // MARK: - Validation of an edited connection

    /// Why a set of edited connection fields can't be saved (nil = fine).
    static func connectionError(hostName: String, user: String, portText: String) -> String? {
        let h = hostName.trimmingCharacters(in: .whitespacesAndNewlines)
        let u = user.trimmingCharacters(in: .whitespacesAndNewlines)
        if h.isEmpty { return "Server address is required." }
        // A space would split into a second ssh config token; a newline would
        // inject a whole directive (SSHConfigManager strips those, but refuse
        // rather than silently mangle what the user typed).
        if h.rangeOfCharacter(from: .whitespacesAndNewlines) != nil {
            return "Server address can't contain spaces."
        }
        if u.isEmpty { return "Username is required." }
        if u.rangeOfCharacter(from: .whitespacesAndNewlines) != nil {
            return "Username can't contain spaces."
        }
        guard let port = parsePort(portText) else {
            return "Port must be a number between 1 and 65535."
        }
        _ = port
        return nil
    }

    /// Parse a port field: empty means the ssh default (22).
    static func parsePort(_ text: String) -> Int? {
        let t = text.trimmingCharacters(in: .whitespaces)
        if t.isEmpty { return 22 }
        guard let n = Int(t), (1...65535).contains(n) else { return nil }
        return n
    }

    // MARK: - Credential display

    /// Dot mask for a stored password of `length` characters, clamped so a very
    /// long passphrase doesn't blow out the row (and a stored-but-unknown length
    /// still shows something).
    static func passwordMask(length: Int) -> String {
        let n = min(max(length, 1), 16)
        return String(repeating: "•", count: n)
    }

    /// Human summary of the stored 2FA secret: which account it belongs to, and
    /// any non-default TOTP parameters worth surfacing.
    static func otpSummary(issuer: String?, account: String?,
                           algorithm: String?, digits: Int?, period: Int?) -> String {
        var parts: [String] = []
        let iss = issuer?.trimmingCharacters(in: .whitespaces) ?? ""
        let acct = account?.trimmingCharacters(in: .whitespaces) ?? ""
        switch (iss.isEmpty, acct.isEmpty) {
        case (false, false): parts.append("\(iss): \(acct)")
        case (false, true):  parts.append(iss)
        case (true, false):  parts.append(acct)
        case (true, true):   break
        }
        // Only mention parameters that DIFFER from the RFC/Duo defaults — always
        // printing "SHA1, 6 digits, 30s" is noise.
        var nonDefault: [String] = []
        if let algorithm, !algorithm.isEmpty, algorithm.uppercased() != "SHA1" {
            nonDefault.append(algorithm.uppercased())
        }
        if let digits, digits != 6 { nonDefault.append("\(digits) digits") }
        if let period, period != 30 { nonDefault.append("\(period)s") }
        if !nonDefault.isEmpty { parts.append(nonDefault.joined(separator: ", ")) }
        if parts.isEmpty { return "Stored (no account details)" }
        return parts.joined(separator: " · ")
    }

    /// Which fields to send to `host_set_credentials`: only what the user
    /// actually typed. Empty/untouched fields come back nil so the daemon keeps
    /// the stored value instead of overwriting it with an empty one.
    static func pendingChanges(newPassword: String,
                               newOTPInput: String,
                               alias: String) -> (password: String?, otpauthURL: String?) {
        let pw = newPassword.isEmpty ? nil : newPassword
        let raw = newOTPInput.trimmingCharacters(in: .whitespacesAndNewlines)
        // Accept a bare base32 secret as well as a full otpauth:// URL — the
        // same normalization the Add Host wizard applies.
        let otp = raw.isEmpty ? nil : (OTPSecret.normalize(input: raw, account: alias) ?? raw)
        return (pw, otp)
    }
}
