import Foundation

/// Parse + format SLURM walltime strings for the compute-allocation countdown.
enum SlurmTime {
    /// Parse a SLURM duration ("2:14:03", "1-12:00:00", "30:45") to seconds.
    /// Returns nil for UNLIMITED / INVALID / NOT_SET / blank / unparseable.
    static func seconds(_ s: String) -> TimeInterval? {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        let upper = t.uppercased()
        if t.isEmpty || upper == "UNLIMITED" || upper == "INVALID"
            || upper == "NOT_SET" || upper == "N/A" { return nil }
        var days = 0
        var rest = t
        if let dash = t.firstIndex(of: "-") {
            guard let d = Int(t[..<dash]) else { return nil }
            days = d
            rest = String(t[t.index(after: dash)...])
        }
        var vals: [Int] = []
        for c in rest.split(separator: ":", omittingEmptySubsequences: false) {
            guard let v = Int(c) else { return nil }
            vals.append(v)
        }
        var h = 0, m = 0, sec = 0
        switch vals.count {
        case 3: h = vals[0]; m = vals[1]; sec = vals[2]
        case 2: m = vals[0]; sec = vals[1]
        case 1: sec = vals[0]
        default: return nil
        }
        return TimeInterval(days * 86400 + h * 3600 + m * 60 + sec)
    }

    /// Compact remaining label, e.g. "2:14:03", "4:09", or "expired".
    static func format(remaining: TimeInterval) -> String {
        if remaining <= 0 { return "expired" }
        let total = Int(remaining)
        let h = total / 3600, m = (total % 3600) / 60, sec = total % 60
        return h > 0
            ? String(format: "%d:%02d:%02d", h, m, sec)
            : String(format: "%d:%02d", m, sec)
    }
}
