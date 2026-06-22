import Foundation

/// Pure decision logic for the Touch ID gate. No LocalAuthentication import so
/// it compiles headlessly into the test bundle.
enum LockCore {
    static func shouldChallenge(enabled: Bool, lastAuth: Date?, now: Date,
                                grace: TimeInterval) -> Bool {
        guard enabled else { return false }
        if let lastAuth, now.timeIntervalSince(lastAuth) < grace { return false }
        return true
    }
}

/// On-disk shape of the synced preferences file (in iCloud Drive).
struct SyncPayload: Codable, Equatable {
    var version: Int                 // reserved for future schema migration; unused by resolve()
    var updatedAt: Double           // epoch seconds (wall clock)
    var values: [String: Bool]
}

enum SyncResolution: Equatable { case applyRemote, writeLocal, noop }

/// Pure last-writer-wins reconcile. No file I/O so it unit-tests headlessly.
enum SyncCore {
    static func resolve(remoteUpdatedAt: Double?, lastAppliedRemoteAt: Double,
                        localLastWriteAt: Double) -> SyncResolution {
        // updatedAt is epoch seconds (Double), NOT Date — keeps this core free of
        // calendar/timezone concerns. On a same-second tie (r == localLastWriteAt)
        // we return .noop; the next real change breaks the tie.
        // Last-writer-wins by wall-clock updatedAt. Cross-device clock skew of a
        // few seconds is acceptable for low-stakes UI prefs — do not over-engineer.
        guard let r = remoteUpdatedAt else { return .writeLocal }   // seed file
        if r > lastAppliedRemoteAt && r > localLastWriteAt { return .applyRemote }
        if localLastWriteAt > r { return .writeLocal }
        return .noop
    }
}
