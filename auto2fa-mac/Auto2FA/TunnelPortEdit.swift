import Foundation

/// Pure validation + diffing for editing a tunnel's ports.
///
/// Foundation-only so it unit-tests headlessly; the popover only renders what
/// this decides.
enum TunnelPortEdit {
    /// Why the drafted ports can't be saved, or nil if they're fine.
    ///
    /// Both fields are required — unlike the credential editor, a blank port has
    /// no "keep the current value" meaning here, because the fields are always
    /// pre-filled with the current ports. A blank one is a typo.
    static func validate(local: String, remote: String) -> String? {
        guard parse(local) != nil else { return "Local port must be a number from 1 to 65535." }
        guard parse(remote) != nil else { return "Remote port must be a number from 1 to 65535." }
        return nil
    }

    /// Which ports actually changed. Sending an unchanged port is harmless but
    /// pointless — and sending only what changed keeps the daemon's "nothing to
    /// change" guard meaningful.
    static func changes(local: String, remote: String,
                        currentLocal: Int, currentRemote: Int) -> (local: Int?, remote: Int?) {
        let l = parse(local)
        let r = parse(remote)
        return (l == currentLocal ? nil : l, r == currentRemote ? nil : r)
    }

    /// A port is 1...65535. Rejects empty, non-numeric, and out-of-range rather
    /// than clamping — a silently-changed port is worse than a refused save.
    static func parse(_ text: String) -> Int? {
        let t = text.trimmingCharacters(in: .whitespaces)
        guard let n = Int(t), (1...65535).contains(n) else { return nil }
        return n
    }
}
