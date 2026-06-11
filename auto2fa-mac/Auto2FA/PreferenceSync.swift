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
    static var fileURL: URL { URL(fileURLWithPath: iCloudDir + "/settings.json") }

    func iCloudAvailable() -> Bool {
        FileManager.default.fileExists(atPath:
            NSHomeDirectory() + "/Library/Mobile Documents/com~apple~CloudDocs")
    }

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
        let remote = readRemote()
        let applied = defaults.double(forKey: PreferenceSync.kLastAppliedRemote)
        let local = defaults.double(forKey: PreferenceSync.kLocalLastWrite)
        switch SyncCore.resolve(remoteUpdatedAt: remote?.updatedAt,
                                lastAppliedRemoteAt: applied, localLastWriteAt: local) {
        case .applyRemote: if let remote { applyRemote(remote) }
        case .writeLocal:  writeLocal()
        case .noop:        lastWrittenSnapshot = currentSnapshot()
        }
    }

    private func onLocalChange() {
        // The sync toggle flipping ON -> immediate reconcile (it is NOT a synced
        // key, so the snapshot diff below would otherwise ignore it).
        let nowEnabled = enabled
        if nowEnabled && !wasEnabled { wasEnabled = true; reconcile(); return }
        wasEnabled = nowEnabled
        guard nowEnabled, !isApplyingRemote else { return }
        // didChangeNotification fires for EVERY key; only write if a synced value
        // actually changed (avoids constant churn + false "local wins").
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

    private func readRemote() -> SyncPayload? {
        let coordinator = NSFileCoordinator()
        var coordErr: NSError?
        var result: SyncPayload?
        coordinator.coordinate(readingItemAt: PreferenceSync.fileURL,
                               options: [], error: &coordErr) { url in
            guard let data = try? Data(contentsOf: url) else { return }
            result = try? JSONDecoder().decode(SyncPayload.self, from: data)
        }
        return result
    }

    private func writeLocal() {
        guard iCloudAvailable() else { return }
        let snap = currentSnapshot()
        let now = Date().timeIntervalSince1970
        let payload = SyncPayload(version: 1, updatedAt: now, values: snap)
        guard let data = try? JSONEncoder().encode(payload) else { return }
        try? FileManager.default.createDirectory(atPath: PreferenceSync.iCloudDir,
            withIntermediateDirectories: true)
        let coordinator = NSFileCoordinator()
        var coordErr: NSError?
        coordinator.coordinate(writingItemAt: PreferenceSync.fileURL,
                               options: .forReplacing, error: &coordErr) { url in
            try? data.write(to: url, options: .atomic)
        }
        lastWrittenSnapshot = snap
        defaults.set(now, forKey: PreferenceSync.kLocalLastWrite)
        defaults.set(now, forKey: PreferenceSync.kLastAppliedRemote)
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
