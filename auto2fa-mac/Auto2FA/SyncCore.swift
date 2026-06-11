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
    var version: Int
    var updatedAt: Double           // epoch seconds (wall clock)
    var values: [String: Bool]
}

enum SyncResolution: Equatable { case applyRemote, writeLocal, noop }

/// Pure last-writer-wins reconcile. No file I/O so it unit-tests headlessly.
enum SyncCore {
    static func resolve(remoteUpdatedAt: Double?, lastAppliedRemoteAt: Double,
                        localLastWriteAt: Double) -> SyncResolution {
        guard let r = remoteUpdatedAt else { return .writeLocal }   // seed file
        if r > lastAppliedRemoteAt && r > localLastWriteAt { return .applyRemote }
        if localLastWriteAt > r { return .writeLocal }
        return .noop
    }
}
