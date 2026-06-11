import Foundation
import AppKit

/// Free, entitlement-less preference sync across the user's Macs: writes an
/// allowlisted JSON of UI preferences into the iCloud Drive folder, which iCloud
/// syncs. NOT NSUbiquitousKeyValueStore (that needs the paid iCloud entitlement).
/// Carries ONLY UI prefs — never secrets or host/tunnel configs.
@MainActor
final class PreferenceSync {
    /// Allowlist of UserDefaults keys to sync. UI preferences only.
    static let syncedKeys: [String] = [
        SettingsKey.notchEnabled, SettingsKey.notchPersistent,
        SettingsKey.notchDoNotDisturb, SettingsKey.autoOpenBrowser,
        SettingsKey.autoRecoverOnWake, SettingsKey.spawnDaemonOnLaunch,
        SettingsKey.compactRows, SettingsKey.requireTouchID,
    ]
    // Device-local bookkeeping (NOT synced).
    private static let kLocalLastWrite = "auto2fa.sync._localLastWriteAt"
    private static let kLastAppliedRemote = "auto2fa.sync._lastAppliedRemoteAt"

    private let defaults = UserDefaults.standard
    private var isApplyingRemote = false
    private var wasEnabled = false
    private var lastWrittenSnapshot: [String: Bool] = [:]
    private var debounceTask: Task<Void, Never>?
    private var observers: [NSObjectProtocol] = []

    var enabled: Bool { defaults.bool(forKey: SettingsKey.syncPrefsViaICloud) }

    static var iCloudDir: String {
        NSHomeDirectory() + "/Library/Mobile Documents/com~apple~CloudDocs/Auto2FA"
    }
    static var fileURL: URL { URL(fileURLWithPath: iCloudDir).appendingPathComponent("settings.json") }

    static func iCloudDriveAvailable() -> Bool {
        FileManager.default.fileExists(atPath:
            NSHomeDirectory() + "/Library/Mobile Documents/com~apple~CloudDocs")
    }

    func iCloudAvailable() -> Bool { PreferenceSync.iCloudDriveAvailable() }

    func start() {
        wasEnabled = enabled
        let nc = NotificationCenter.default
        observers.append(nc.addObserver(forName: UserDefaults.didChangeNotification,
            object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.onLocalChange() }
        })
        observers.append(nc.addObserver(forName: NSApplication.didBecomeActiveNotification,
            object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.reconcile() }
        })
        reconcile()
    }

    func reconcile() {
        guard enabled, iCloudAvailable() else { return }
        let url = PreferenceSync.fileURL
        // The coordinated iCloud read can BLOCK (waiting for other coordinators or
        // for iCloud to materialize the file) — do it OFF the main thread, then
        // hop back to the actor to decide + apply.
        Task.detached(priority: .utility) {
            let remote = PreferenceSync.readCoordinated(from: url)
            await MainActor.run { [weak self] in
                guard let self, self.enabled else { return }
                let applied = self.defaults.double(forKey: PreferenceSync.kLastAppliedRemote)
                let local = self.defaults.double(forKey: PreferenceSync.kLocalLastWrite)
                switch SyncCore.resolve(remoteUpdatedAt: remote?.updatedAt,
                                        lastAppliedRemoteAt: applied, localLastWriteAt: local) {
                case .applyRemote: if let remote { self.applyRemote(remote) }
                case .writeLocal:  self.writeLocal()
                case .noop:        self.lastWrittenSnapshot = self.currentSnapshot()
                }
            }
        }
    }

    private func onLocalChange() {
        let nowEnabled = enabled
        // Toggle ON: the sync flag is NOT a synced key, so the snapshot diff below
        // would ignore it — reconcile immediately instead.
        if nowEnabled && !wasEnabled { wasEnabled = true; reconcile(); return }
        // Toggle OFF: stop syncing AND cancel any in-flight debounced write so a
        // write doesn't land ~1s after the user turned sync off.
        if !nowEnabled { debounceTask?.cancel(); wasEnabled = false; return }
        wasEnabled = nowEnabled
        // `isApplyingRemote` is belt-and-suspenders; the ACTUAL write-loop guard is
        // the snapshot diff below (applyRemote/writeLocal set lastWrittenSnapshot, so
        // the change notification they cause finds no synced-value delta and stops).
        guard !isApplyingRemote else { return }
        let snap = currentSnapshot()
        guard snap != lastWrittenSnapshot else { return }
        debounceTask?.cancel()
        debounceTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_000_000_000)
            guard let self, !Task.isCancelled else { return }
            self.writeLocal()
        }
    }

    private func currentSnapshot() -> [String: Bool] {
        var s: [String: Bool] = [:]
        for k in PreferenceSync.syncedKeys { s[k] = defaults.bool(forKey: k) }
        return s
    }

    private func writeLocal() {
        guard enabled, iCloudAvailable() else { return }
        let snap = currentSnapshot()
        let now = Date().timeIntervalSince1970
        guard let data = try? JSONEncoder().encode(
            SyncPayload(version: 1, updatedAt: now, values: snap)) else { return }
        // Update bookkeeping synchronously on the actor FIRST, so the snapshot-diff
        // guard + timestamps are consistent before any further change notification
        // fires (prevents a self-triggered re-write).
        lastWrittenSnapshot = snap
        defaults.set(now, forKey: PreferenceSync.kLocalLastWrite)
        defaults.set(now, forKey: PreferenceSync.kLastAppliedRemote)
        // The coordinated iCloud write is blocking — do it OFF the main thread.
        let url = PreferenceSync.fileURL
        let dir = PreferenceSync.iCloudDir
        Task.detached(priority: .utility) {
            PreferenceSync.writeCoordinated(data: data, to: url, dir: dir)
        }
    }

    /// Coordinated iCloud read — runs OFF the actor (nonisolated, no actor state).
    nonisolated static func readCoordinated(from url: URL) -> SyncPayload? {
        let coordinator = NSFileCoordinator()
        var coordErr: NSError?
        var result: SyncPayload?
        coordinator.coordinate(readingItemAt: url, options: [], error: &coordErr) { u in
            guard let data = try? Data(contentsOf: u) else { return }
            result = try? JSONDecoder().decode(SyncPayload.self, from: data)
        }
        return result
    }

    /// Coordinated iCloud write — runs OFF the actor (nonisolated, no actor state).
    nonisolated static func writeCoordinated(data: Data, to url: URL, dir: String) {
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let coordinator = NSFileCoordinator()
        var coordErr: NSError?
        coordinator.coordinate(writingItemAt: url, options: .forReplacing, error: &coordErr) { u in
            try? data.write(to: u, options: .atomic)
        }
    }

    private func applyRemote(_ payload: SyncPayload) {
        isApplyingRemote = true
        for k in PreferenceSync.syncedKeys {
            if let v = payload.values[k] { defaults.set(v, forKey: k) }
        }
        lastWrittenSnapshot = currentSnapshot()
        defaults.set(payload.updatedAt, forKey: PreferenceSync.kLastAppliedRemote)
        isApplyingRemote = false
    }
}
