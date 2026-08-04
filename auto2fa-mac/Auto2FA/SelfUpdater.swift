import Foundation
import AppKit
import CryptoKit

/// Downloads a release and installs it over the running app — the one-click
/// update. All decisions live in the unit-tested `SelfUpdateCore`; this type
/// owns the side-effects (network, hdiutil, ditto, the detached swap) and the
/// progress state the UI observes.
///
/// A singleton so the menu bar and the About pane drive and observe the SAME
/// update: starting one from the menu and then opening Settings must show the
/// running download, not start a second one.
///
/// # Why it can't just copy over itself
///
/// A running bundle can't replace its own directory, and the daemon inside it
/// must be stopped in a very specific way — SIGKILL, so its SSH ControlMasters
/// survive and the replacement adopts them (a graceful stop tears them down and
/// costs a fresh 2FA login on every host). So the expensive, failure-prone work
/// (download, verify, mount, copy) happens HERE, while the app is alive and can
/// report errors; only a short swap-and-relaunch script outlives the app.
@MainActor
final class SelfUpdater: ObservableObject {
    static let shared = SelfUpdater()

    enum Phase: Equatable {
        case idle
        case preparing
        case downloading(received: Int64, total: Int64)
        case verifying
        case installing
        /// Everything is staged; the app is about to quit and relaunch.
        case relaunching
        case failed(String)

        var isBusy: Bool {
            switch self {
            case .idle, .failed: return false
            default: return true
            }
        }
    }

    @Published private(set) var phase: Phase = .idle

    /// Where the swap script logs, so a failed update is diagnosable after the
    /// app that started it is gone.
    static let logPath = "/tmp/ssh2fa-update.log"

    private var workDir: URL?

    private init() {}

    /// True when this install can be replaced in place. The About pane uses it
    /// to decide between the one-click button and the manual instructions.
    static var blocker: SelfUpdateCore.Blocker? {
        let path = Bundle.main.bundlePath
        var readOnly = false
        if let vals = try? Bundle.main.bundleURL.resourceValues(forKeys: [.volumeIsReadOnlyKey]) {
            readOnly = vals.volumeIsReadOnly ?? false
        }
        return SelfUpdateCore.blocker(
            bundlePath: path,
            isReadOnlyVolume: readOnly,
            isWritable: FileManager.default.isWritableFile(atPath:))
    }

    /// Download `version` and install it, then quit and relaunch.
    ///
    /// Safe to call twice — a second call while one is running is ignored
    /// rather than starting a competing download into the same staging path.
    func install(version advertised: String) async {
        guard !phase.isBusy else { return }
        phase = .preparing

        if let b = Self.blocker {
            phase = .failed(Self.explain(b))
            return
        }
        do {
            try await runInstall(advertised: advertised)
        } catch is CancellationError {
            cleanUp()
            phase = .idle
        } catch {
            cleanUp()
            phase = .failed((error as? UpdateError)?.text ?? error.localizedDescription)
        }
    }

    /// Clear a failure so the button returns to "Update & Relaunch".
    func reset() {
        guard !phase.isBusy else { return }
        phase = .idle
    }

    // MARK: - The pipeline

    private struct UpdateError: Error { let text: String }

    private func runInstall(advertised: String) async throws {
        let fm = FileManager.default

        // 0. Fresh staging area, and sweep any left by an earlier attempt.
        Self.sweepOldWorkDirs()
        let work = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("ssh2fa-update-\(UUID().uuidString.prefix(8))")
        try fm.createDirectory(at: work, withIntermediateDirectories: true)
        workDir = work

        // 1. Resolve the exact release the UI advertised and authenticate its
        // signed manifest BEFORE downloading executable content.
        let package = try await Self.fetchReleasePackage(advertised: advertised)
        let manifestData = try await Self.fetchManifest(package.manifestURL)
        let manifest: UpdateSigningCore.Manifest
        switch UpdateSigningCore.decodeManifest(manifestData) {
        case .success(let decoded): manifest = decoded
        case .failure:
            throw UpdateError(text: String(localized: "This update doesn't have a valid SSH2FA signature, so it wasn't installed."))
        }
        guard manifest.validationProblem(
            advertisedVersion: advertised,
            trustedKeys: UpdateSigningCore.trustedPublicKeys) == nil else {
            throw UpdateError(text: String(localized: "This update doesn't have a valid SSH2FA signature, so it wasn't installed."))
        }

        // 2. Download it, streaming progress into the UI.
        phase = .downloading(received: 0, total: manifest.size)
        let dmg = work.appendingPathComponent("SSH2FA.dmg")
        try await download(asset: package.dmg, to: dmg)

        // 3. Integrity comes from the project-signed manifest, independent of
        // Apple's paid trust chain. GitHub's own digest remains a second check.
        phase = .verifying
        let actual = try Self.sha256(of: dmg)
        let attributes = try fm.attributesOfItem(atPath: dmg.path)
        let actualSize = (attributes[.size] as? NSNumber)?.int64Value ?? -1
        guard manifest.digestMatches(actual, actualSize: actualSize),
              SelfUpdateCore.digestOK(expected: package.dmg.sha256, actual: actual) else {
            throw UpdateError(text: String(localized: "The download didn't match SSH2FA's signed checksum, so it wasn't installed. Try again."))
        }

        // 4. Mount, vet, and copy the new bundle NEXT TO the current one, so the
        //    swap that happens after we quit is a rename, not a copy.
        phase = .installing
        let mnt = work.appendingPathComponent("mnt")
        try fm.createDirectory(at: mnt, withIntermediateDirectories: true)
        let attach = Self.run("/usr/bin/hdiutil",
                              ["attach", dmg.path, "-nobrowse", "-readonly", "-quiet",
                               "-mountpoint", mnt.path],
                              timeout: 120)
        guard attach.code == 0 else {
            throw UpdateError(text: String(localized: "Couldn't open the downloaded disk image."))
        }
        defer { Self.detach(mnt) }

        let staged = mnt.appendingPathComponent(SelfUpdateCore.appBundleName)
        try Self.vet(staged: staged, advertised: advertised,
                     advertisedBuild: manifest.build)

        let target = Bundle.main.bundleURL
        let stamp = UUID().uuidString.prefix(8)
        let incoming = target.deletingLastPathComponent()
            .appendingPathComponent("\(target.lastPathComponent).new-\(stamp)")
        try? fm.removeItem(at: incoming)
        let copy = Self.run("/usr/bin/ditto", [staged.path, incoming.path], timeout: 300)
        guard copy.code == 0 else {
            try? fm.removeItem(at: incoming)
            throw UpdateError(text: String(localized: "Couldn't write the new version next to the current one: \(copy.output)"))
        }
        // Downloaded code can carry the Gatekeeper quarantine flag, which would
        // block the embedded (un-notarized) daemon at exec.
        Self.run("/usr/bin/xattr", ["-dr", "com.apple.quarantine", incoming.path], timeout: 60)

        // 5. Hand the swap to a detached script and get out of the way.
        let old = target.deletingLastPathComponent()
            .appendingPathComponent("\(target.lastPathComponent).old-\(stamp)")
        let script = SelfUpdateCore.swapScript(
            appPID: ProcessInfo.processInfo.processIdentifier,
            target: target.path,
            staged: incoming.path,
            old: old.path,
            daemonPath: DaemonProcess.bundledDaemonURL()?.path,
            logPath: Self.logPath
        )
        let scriptURL = work.appendingPathComponent("swap.sh")
        try script.write(to: scriptURL, atomically: true, encoding: .utf8)

        phase = .relaunching
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = [scriptURL.path]
        // Detached on purpose: it must outlive us. Do NOT waitUntilExit.
        do {
            try p.run()
        } catch {
            try? fm.removeItem(at: incoming)
            throw UpdateError(text: String(localized: "Couldn't start the installer step: \(error.localizedDescription)"))
        }

        // Give the UI a beat to render "Restarting…", then quit so the script
        // can take over. The staging dir is deliberately NOT cleaned here — the
        // script still needs it.
        try? await Task.sleep(nanoseconds: 600_000_000)
        NSApp.terminate(nil)
    }

    // MARK: - Steps

    /// The exact tagged release the UI offered, including its DMG and signed
    /// manifest. Unsigned legacy releases are deliberately not self-installable.
    private static func fetchReleasePackage(advertised: String) async throws
        -> SelfUpdateCore.ReleasePackage {
        guard let apiURL = SelfUpdateCore.releaseAPIURL(advertisedVersion: advertised) else {
            throw UpdateError(text: String(localized: "SSH2FA couldn't identify that release safely."))
        }
        var req = URLRequest(url: apiURL)
        req.timeoutInterval = 20
        req.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        let (data, resp) = try await URLSession.shared.data(for: req)
        let code = (resp as? HTTPURLResponse)?.statusCode ?? 0
        guard code == 200 else {
            throw UpdateError(text: String(localized: "GitHub returned HTTP \(code) when looking for the download."))
        }
        guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let assets = obj["assets"] as? [[String: Any]],
              let package = SelfUpdateCore.pickReleasePackage(assets: assets) else {
            throw UpdateError(text: String(localized: "That release doesn't include a signed SSH2FA update — update manually this time."))
        }
        return package
    }

    private static func fetchManifest(_ url: URL) async throws -> Data {
        var request = URLRequest(url: url)
        request.timeoutInterval = 20
        let (data, response) = try await URLSession.shared.data(for: request)
        let code = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard code == 200, data.count <= 64 * 1024 else {
            throw UpdateError(text: String(localized: "SSH2FA couldn't verify this update's signature."))
        }
        return data
    }

    /// Download with live progress. Uses a download task (not `data(for:)`) so
    /// the 7 MB image streams to disk with byte-accurate progress instead of
    /// buffering in memory behind a spinner.
    private func download(asset: SelfUpdateCore.ReleaseAsset, to dest: URL) async throws {
        let progress = DownloadProgress { [weak self] received, total in
            Task { @MainActor in
                guard let self, case .downloading = self.phase else { return }
                self.phase = .downloading(received: received,
                                          total: total > 0 ? total : asset.size)
            }
        }
        var req = URLRequest(url: asset.url)
        req.timeoutInterval = 120
        let (tmp, resp) = try await URLSession.shared.download(for: req, delegate: progress)
        let code = (resp as? HTTPURLResponse)?.statusCode ?? 0
        guard code == 200 else {
            throw UpdateError(text: String(localized: "The download failed with HTTP \(code)."))
        }
        try? FileManager.default.removeItem(at: dest)
        try FileManager.default.moveItem(at: tmp, to: dest)
    }

    /// Streamed SHA-256 — never loads the whole image into memory.
    private static func sha256(of url: URL) throws -> String {
        let h = try FileHandle(forReadingFrom: url)
        defer { try? h.close() }
        var hasher = SHA256()
        while let chunk = try h.read(upToCount: 1 << 20), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    /// Refuse anything that isn't a valid, newer SSH2FA before it replaces us.
    private static func vet(staged: URL,
                            advertised: String,
                            advertisedBuild: String) throws {
        let fm = FileManager.default
        guard fm.fileExists(atPath: staged.path) else {
            throw UpdateError(text: String(localized: "The disk image didn't contain SSH2FA."))
        }
        guard let info = NSDictionary(contentsOf:
                staged.appendingPathComponent("Contents/Info.plist")),
              let bid = info["CFBundleIdentifier"] as? String,
              let ver = info["CFBundleShortVersionString"] as? String,
              let build = info["CFBundleVersion"] as? String else {
            throw UpdateError(text: String(localized: "The downloaded app is missing its version information."))
        }
        if let why = SelfUpdateCore.rejectStagedApp(
            bundleID: bid, version: ver, build: build,
            currentVersion: UpdateChecker.currentVersion,
            advertised: advertised, advertisedBuild: advertisedBuild) {
            throw UpdateError(text: why)
        }
    }

    // MARK: - Housekeeping

    private func cleanUp() {
        if let w = workDir { Self.detach(w.appendingPathComponent("mnt")) ; try? FileManager.default.removeItem(at: w) }
        workDir = nil
    }

    /// Remove staging directories left behind by an earlier attempt (a crash, or
    /// a swap script that finished after the app was gone).
    private static func sweepOldWorkDirs() {
        let fm = FileManager.default
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
        guard let items = try? fm.contentsOfDirectory(at: tmp,
                                                      includingPropertiesForKeys: nil) else { return }
        for i in items where i.lastPathComponent.hasPrefix("ssh2fa-update-") {
            detach(i.appendingPathComponent("mnt"))
            try? fm.removeItem(at: i)
        }
    }

    /// Unmount, escalating to `-force` — a mount left attached would keep the
    /// staging directory undeletable and confuse the next attempt.
    private static func detach(_ mnt: URL) {
        guard FileManager.default.fileExists(atPath: mnt.path) else { return }
        if run("/usr/bin/hdiutil", ["detach", mnt.path, "-quiet"], timeout: 60).code != 0 {
            run("/usr/bin/hdiutil", ["detach", mnt.path, "-force", "-quiet"], timeout: 60)
        }
    }

    /// Run a tool with a HARD deadline. Every external command in this file is
    /// one an unlucky machine can hang on (a wedged mount, a stalled ditto);
    /// none of them may hang the update forever.
    @discardableResult
    private static func run(_ path: String, _ args: [String],
                            timeout: TimeInterval) -> (code: Int32, output: String) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        do { try p.run() } catch { return (-1, error.localizedDescription) }
        let deadline = Date().addingTimeInterval(timeout)
        while p.isRunning && Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
        if p.isRunning {
            kill(p.processIdentifier, SIGKILL)
            return (-1, "timed out after \(Int(timeout))s")
        }
        let out = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return (p.terminationStatus, out.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    /// User-facing explanation for why this install can't self-update.
    static func explain(_ b: SelfUpdateCore.Blocker) -> String {
        switch b {
        case .notAnAppBundle:
            return String(localized: "This build isn't running from an app bundle, so it can't update itself.")
        case .readOnlyLocation:
            return String(localized: "SSH2FA is running from a read-only location (probably still inside the disk image). Drag it to your Applications folder first, then updates install with one click.")
        case .translocated:
            return String(localized: "macOS is running SSH2FA from a temporary read-only copy. Move the app to your Applications folder and open it from there, then updates install with one click.")
        case .noPermission(let dir):
            return String(localized: "SSH2FA doesn't have permission to update itself in \(dir) — it may have been installed by another user.")
        }
    }
}

/// Reports download progress. `URLSession`'s async `download(for:delegate:)`
/// gives byte counts only through a delegate, so this is the smallest possible
/// one: it forwards to a closure and holds no state of its own.
private final class DownloadProgress: NSObject, URLSessionTaskDelegate, URLSessionDownloadDelegate {
    private let onProgress: @Sendable (Int64, Int64) -> Void

    init(onProgress: @escaping @Sendable (Int64, Int64) -> Void) {
        self.onProgress = onProgress
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask,
                    didWriteData bytesWritten: Int64,
                    totalBytesWritten: Int64,
                    totalBytesExpectedToWrite: Int64) {
        onProgress(totalBytesWritten, totalBytesExpectedToWrite)
    }

    /// Required by the protocol; the async API takes ownership of the file, so
    /// there is nothing to do here.
    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask,
                    didFinishDownloadingTo location: URL) {}
}
