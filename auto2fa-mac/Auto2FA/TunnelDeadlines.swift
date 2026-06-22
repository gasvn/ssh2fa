import Foundation

/// Per-tunnel compute-allocation expiry times (epoch seconds), persisted so the
/// countdown survives app restarts. Set when the user picks a SLURM node whose
/// job reports a finite TIME_LEFT.
enum TunnelDeadlines {
    static let key = "auto2fa.tunnelDeadlines"

    static func set(_ name: String, endsAt: Date) {
        var d = raw(); d[name] = endsAt.timeIntervalSince1970
        UserDefaults.standard.set(d, forKey: key)
    }

    static func endsAt(_ name: String) -> Date? {
        guard let e = raw()[name] else { return nil }
        return Date(timeIntervalSince1970: e)
    }

    static func clear(_ name: String) {
        var d = raw(); d.removeValue(forKey: name)
        UserDefaults.standard.set(d, forKey: key)
    }

    private static func raw() -> [String: Double] {
        (UserDefaults.standard.dictionary(forKey: key) as? [String: Double]) ?? [:]
    }
}
