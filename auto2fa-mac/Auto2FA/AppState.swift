import Foundation
import SwiftUI

/// Observable mirror of daemon state. Lives for the lifetime of the app.
///
/// Owns one `BackendClient`. Periodically pulls full snapshots AND reacts to
/// pushed events for instant updates. Falls back to polling if the daemon
/// hasn't pushed an event in a while.
/// Which modal sheet (if any) the main window is showing.
enum ActiveSheet: Identifiable, Equatable {
    case newTunnel
    case nodePicker(tunnelName: String)
    case customNode(tunnelName: String)
    case confirmDelete(tunnelName: String)
    case addHost(prefillAlias: String?)
    case importHosts

    var id: String {
        switch self {
        case .newTunnel: return "newTunnel"
        case .nodePicker(let n): return "nodePicker:\(n)"
        case .customNode(let n): return "customNode:\(n)"
        case .confirmDelete(let n): return "confirmDelete:\(n)"
        case .addHost(let a): return "addHost:\(a ?? "")"
        case .importHosts: return "importHosts"
        }
    }
}

/// A newer release surfaced by the notify-only update reminder.
struct AvailableUpdate: Equatable {
    let version: String
    let url: URL
}

@MainActor
final class AppState: ObservableObject {
    @Published var hosts: [SSHHost] = [] {
        didSet { celebrateFirstConnectIfNeeded() }
    }
    @Published var tunnels: [Tunnel] = []
    @Published var connectionError: String?
    /// Global search text driven by the toolbar field; read by HostsView and
    /// TunnelsView to filter their lists. Empty = show everything.
    @Published var searchQuery: String = ""
    @Published var notchPresenter: NotchPresenter = NotchPresenter()
    let persistentNotch: PersistentNotchController = PersistentNotchController()
    @Published var activeSheet: ActiveSheet?
    /// Cached parse of ~/.ssh/config. Refreshed on each reloadAll + when the
    /// import sheet opens, so the per-row drift check and the import list read
    /// memory instead of hitting disk on every SwiftUI render pass.
    @Published private(set) var parsedConfig: ParsedSSHConfig = .empty
    /// Names of hosts/tunnels with an action currently in flight (toggle,
    /// pick_node, delete). UI uses this to swap the action button for a
    /// spinner and overlay a "Working…" status so the user sees that their
    /// click was received — daemon-side operations can take 10-30s while
    /// they probe the local port / wait for SSH to settle.
    @Published var inFlightHosts: Set<String> = []
    @Published var inFlightTunnels: Set<String> = []
    /// Snapshot of the most recently deleted tunnel, kept ~8s so the user
    /// can hit Undo from the snackbar. Auto-clears on timer or on the next
    /// successful delete.
    @Published var undoableDelete: Tunnel?
    private var undoExpireTask: Task<Void, Never>?
    /// A newer release found by the notify-only background check. Drives the
    /// menu-bar "Update available" item + status-item marker. nil = up to date
    /// (or not yet checked / check disabled).
    @Published var availableUpdate: AvailableUpdate?
    private var updateLoopStarted = false

    let client = BackendClient()
    private var eventTask: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?
    /// Consecutive background-reload failures. A single transient timeout (a
    /// busy daemon, a brief blip) is NOT shown to the user — only a sustained
    /// run of failures surfaces a (friendly) banner. Reset on any success.
    private var reloadFailStreak = 0
    /// bootstrap() runs again on the owned-daemon-respawn path, where the
    /// connection watcher already shows "Daemon reconnected". Show the cold-launch
    /// "ready" toast only ONCE so a respawn doesn't fire two toasts.
    private var hasShownReadyToast = false

    // MARK: - Update reminder (notify-only)

    /// Start the daily background "is there a newer release?" reminder. Safe to
    /// call repeatedly — only the first call starts the loop. Fire-and-forget:
    /// it sleeps before the first check so it never competes with launch, and
    /// every check is gated to once/24h (and to the user toggle) inside.
    func startUpdateReminderLoop() {
        guard !updateLoopStarted else { return }
        updateLoopStarted = true
        // Re-hydrate a previously-found update immediately so the menu-bar marker
        // survives a relaunch (it would otherwise disappear until the next 24h
        // check). Off the network — just reads UserDefaults.
        loadPersistedAvailableUpdate()
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 8_000_000_000) // let launch settle
            await self?.autoCheckForUpdatesIfDue()
            while !Task.isCancelled {
                // Tick every 6h; the 24h throttle inside decides if it actually
                // hits the network. Cheap, and catches long-running sessions.
                try? await Task.sleep(nanoseconds: 6 * 60 * 60 * 1_000_000_000)
                await self?.autoCheckForUpdatesIfDue()
            }
        }
    }

    /// One throttled, opt-out-able update check. Notify-only: on finding a newer
    /// (un-skipped) release it surfaces `availableUpdate` (menu-bar marker, About
    /// pane) and raises at most one notification per version. Never downloads or
    /// installs. All decisions go through the unit-tested `UpdateCheckCore`.
    func autoCheckForUpdatesIfDue(now: Date = Date()) async {
        let d = UserDefaults.standard
        let enabled = d.object(forKey: SettingsKey.autoCheckUpdates) as? Bool ?? true
        guard enabled else { setAvailableUpdate(nil); return } // opted out → no marker
        let last = (d.object(forKey: SettingsKey.lastUpdateCheckAt) as? Double)
            .map { Date(timeIntervalSince1970: $0) }
        guard UpdateCheckCore.shouldCheckNow(enabled: enabled, lastCheck: last,
                                             now: now, interval: 24 * 60 * 60) else { return }
        d.set(now.timeIntervalSince1970, forKey: SettingsKey.lastUpdateCheckAt)

        switch await UpdateChecker.fetchLatest() {
        case .updateAvailable(let latest, let url):
            let current = UpdateChecker.currentVersion
            let skipped = d.string(forKey: SettingsKey.skippedUpdateVersion)
            guard UpdateCheckCore.shouldSurface(latest: latest, current: current, skipped: skipped) else {
                setAvailableUpdate(nil); return    // skipped or not actually newer
            }
            setAvailableUpdate(AvailableUpdate(version: latest, url: url))
            let lastNotified = d.string(forKey: SettingsKey.lastNotifiedVersion)
            if UpdateCheckCore.shouldNotify(latest: latest, current: current,
                                            lastNotified: lastNotified, skipped: skipped) {
                // Record "already notified" ONLY if the toast actually posted
                // (permission granted) — else let it retry next check so the
                // first-ever reminder isn't silently lost when the auth prompt
                // raced the post.
                if await MacNotifications.postUpdateAvailable(version: latest, url: url) {
                    d.set(latest, forKey: SettingsKey.lastNotifiedVersion)
                }
            }
        case .upToDate:
            setAvailableUpdate(nil)   // clear a stale marker if the user updated
        case .idle, .checking, .failed:
            break                     // transient — leave any existing marker as-is
        }
    }

    /// Restore `availableUpdate` from a prior session (set on disk), unless the
    /// user has since skipped that version, auto-check is off, or it's no longer
    /// newer than the running build.
    private func loadPersistedAvailableUpdate() {
        let d = UserDefaults.standard
        guard (d.object(forKey: SettingsKey.autoCheckUpdates) as? Bool ?? true) else { return }
        guard let v = d.string(forKey: SettingsKey.availableUpdateVersion),
              let s = d.string(forKey: SettingsKey.availableUpdateURL),
              let url = URL(string: s) else { return }
        let skipped = d.string(forKey: SettingsKey.skippedUpdateVersion)
        if UpdateCheckCore.shouldSurface(latest: v, current: UpdateChecker.currentVersion, skipped: skipped) {
            availableUpdate = AvailableUpdate(version: v, url: url)
        } else {
            setAvailableUpdate(nil)
        }
    }

    /// Set + persist (or clear) the surfaced update so it survives relaunch.
    private func setAvailableUpdate(_ u: AvailableUpdate?) {
        availableUpdate = u
        let d = UserDefaults.standard
        if let u {
            d.set(u.version, forKey: SettingsKey.availableUpdateVersion)
            d.set(u.url.absoluteString, forKey: SettingsKey.availableUpdateURL)
        } else {
            d.removeObject(forKey: SettingsKey.availableUpdateVersion)
            d.removeObject(forKey: SettingsKey.availableUpdateURL)
        }
    }

    /// "Skip this version" — stop surfacing + notifying for `version`.
    func skipUpdate(_ version: String) {
        UserDefaults.standard.set(version, forKey: SettingsKey.skippedUpdateVersion)
        setAvailableUpdate(nil)
    }

    /// React to the General → "Automatically check for updates" toggle: turning
    /// it off clears the marker immediately; turning it on re-checks.
    func updateAutoCheckChanged(enabled: Bool) {
        if enabled {
            UserDefaults.standard.removeObject(forKey: SettingsKey.lastUpdateCheckAt) // re-check now
            Task { await autoCheckForUpdatesIfDue() }
        } else {
            setAvailableUpdate(nil)
        }
    }

    func bootstrap() async {
        NSLog("[SSH2FA] bootstrap: connecting to daemon")
        do {
            try await client.connect()
            connectionError = nil
            NSLog("[SSH2FA] bootstrap: connected OK")
            if !hasShownReadyToast {
                hasShownReadyToast = true
                notchPresenter.show(
                    systemImage: "bolt.fill",
                    title: "SSH2FA ready",
                    description: "Connected to daemon",
                    tint: .green
                )
            }
        } catch {
            NSLog("[SSH2FA] bootstrap: connect failed: \(error.localizedDescription)")
            connectionError = "Daemon unreachable: \(error.localizedDescription). " +
                              "Is ssh2fa-daemon running?"
            // DON'T return — start the watcher/poll machinery anyway. The
            // old early-return was a dead end: launching the app during a
            // daemon-down window (deploys SIGKILL it; launchd respawns ~10s
            // later) left a permanent "Daemon unreachable" banner that only
            // an app relaunch cleared. The connection watcher + poll
            // fallback reconnect and clear the banner on their own.
        }
        await reloadAll()
        NSLog("[SSH2FA] bootstrap: loaded \(hosts.count) hosts, \(tunnels.count) tunnels")
        startEventTask()
        startConnectionWatcher()
        startPollFallback()
    }

    /// Listen for daemon disconnect / reconnect cycles. On disconnect we
    /// surface a banner + show a notch toast and kick off a backoff retry
    /// in a SEPARATE Task — otherwise the watcher loop blocks for the
    /// full backoff window (up to ~2 minutes) and the `true` yielded on
    /// reconnect arrives but isn't consumed until then.
    private var connWatcherTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private func startConnectionWatcher() {
        connWatcherTask?.cancel()
        connWatcherTask = Task { [weak self] in
            // Fresh per-subscriber stream: cancelling the previous watcher task
            // FINISHED its stream (AsyncStream is single-use), so re-iterating
            // a shared stream after the first daemon respawn silently dropped
            // all future disconnect notifications.
            guard let client = self?.client else { return }
            let stream = await client.connectionStateStream()
            for await connected in stream {
                guard let self else { return }
                if connected {
                    await MainActor.run {
                        self.connectionError = nil
                        // If the cold-launch bootstrap never connected (daemon was
                        // down at launch), THIS is the first-ever connect — show
                        // "ready", not "reconnected". Otherwise it's a true reconnect.
                        if !self.hasShownReadyToast {
                            self.hasShownReadyToast = true
                            self.notchPresenter.show(
                                systemImage: "bolt.fill",
                                title: "SSH2FA ready",
                                description: "Connected to daemon",
                                tint: .green
                            )
                        } else {
                            self.notchPresenter.show(
                                systemImage: "bolt.fill",
                                title: "Daemon reconnected",
                                description: "state restored",
                                tint: .green
                            )
                        }
                    }
                    await self.reloadAll()
                    self.startEventTask()  // re-subscribe events on the new socket
                } else {
                    await MainActor.run {
                        self.connectionError = "Daemon disconnected — retrying…"
                        self.notchPresenter.show(
                            systemImage: "wifi.slash",
                            title: "Daemon lost",
                            description: "auto-reconnecting…",
                            tint: .orange
                        )
                    }
                    // Run reconnect detached so the watcher loop keeps
                    // pulling state changes from the stream.
                    self.reconnectTask?.cancel()
                    self.reconnectTask = Task { [weak self] in
                        guard let self else { return }
                        // First: if we OWNED the daemon process and it's
                        // dead (not just the socket), respawn before
                        // hammering the socket with retries that can
                        // never succeed.
                        //
                        // Loop the respawn with backoff — previously a single
                        // failed respawn left the app in a permanently-dead
                        // "Daemon respawn failed" state until manual restart.
                        let delays: [UInt64] = [2, 5, 10, 30, 60, 60, 60]
                        for delay in delays {
                            if Task.isCancelled { return }
                            if let respawn = await DaemonProcess.shared.respawnIfOwnedDaemonCrashed() {
                                switch respawn {
                                case .alreadyRunning, .spawned:
                                    NSLog("[SSH2FA] daemon respawned after crash")
                                    await self.bootstrap()
                                    return
                                case .failed(let reason):
                                    NSLog("[SSH2FA] daemon respawn failed: \(reason), retrying")
                                    await MainActor.run {
                                        self.connectionError = "Daemon respawn failed (will retry): \(reason)"
                                    }
                                    try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                                    continue
                                }
                            }
                            // We don't own a daemon — fall back to socket-
                            // level reconnect (LaunchAgent / external daemon).
                            break
                        }
                        // If every backoff attempt failed, say so plainly
                        // instead of leaving the "retrying…" banner up forever.
                        // (On success reconnectWithBackoff yields true, which
                        // the watcher turns into connectionError = nil.)
                        let ok = await self.client.reconnectWithBackoff()
                        if !ok && !Task.isCancelled {
                            await MainActor.run {
                                self.connectionError =
                                    "Couldn't reconnect to the daemon. Restart SSH2FA, or check /tmp/ssh2fa_daemon.log."
                            }
                        }
                    }
                }
            }
        }
    }

    /// The first time ANY host reaches Connected, show a one-off celebratory
    /// notch with the "now just `ssh`" next step. Gated by a UserDefaults flag
    /// so it never repeats.
    private func celebrateFirstConnectIfNeeded() {
        let key = "auto2fa.firstConnectShown"
        guard !UserDefaults.standard.bool(forKey: key),
              let h = hosts.first(where: { $0.displayState == .connected })
        else { return }
        UserDefaults.standard.set(true, forKey: key)
        notchPresenter.show(
            systemImage: "checkmark.seal.fill",
            title: "Connected!",
            description: "\(h.host) is live — open a Terminal from its row. No code to type.",
            tint: .green)
    }

    func reloadAll() async {
        let isFirstLoad = self.tunnels.isEmpty && lastNotchSignature.isEmpty
        do {
            self.hosts = try await client.listHosts()
            self.tunnels = try await client.listTunnels()
            updateDockBadge()
            checkDeadlines()
            // On the very first reload at app launch, seed the dedup map
            // with every tunnel's current status — otherwise the first
            // batch of TUNNEL_STATUS_CHANGED events would each be treated
            // as "new alive transition" and we'd fire N "Connected X"
            // notches in rapid succession for tunnels that have been
            // alive for hours.
            if isFirstLoad {
                for t in self.tunnels {
                    self.lastNotchSignature[t.name] = notchSignature(status: t.status, lastMsg: t.lastMsg)
                }
            }
            // Success → the daemon is reachable. Clear any stale transient
            // banner and reset the failure streak.
            reloadFailStreak = 0
            if connectionError != nil { connectionError = nil }
            refreshConfigCache()
            syncManagedSSHConfig()
        } catch {
            // A single background-poll timeout is almost always a transient blip
            // (busy daemon, brief hiccup, one lost request) — NOT a real outage,
            // and the next 5s poll usually succeeds. Don't alarm the user on the
            // first miss; only surface a FRIENDLY banner after several
            // consecutive failures. Genuine socket disconnects are handled
            // separately by the connection watcher (startConnectionWatcher).
            reloadFailStreak += 1
            NSLog("[SSH2FA] reloadAll failed (streak \(reloadFailStreak)): \(error.localizedDescription)")
            if reloadFailStreak >= 3 {
                connectionError = "Daemon is slow to respond — retrying…"
            }
        }
    }

    // MARK: - Compute-allocation expiry warnings

    /// Tunnel names already warned about imminent expiry (re-armed when the
    /// deadline moves back out past the threshold or is cleared).
    private var warnedDeadlines: Set<String> = []

    /// Fire a one-time notch warning ~10 min before a running tunnel's compute
    /// allocation expires. Called on every reload (≤5s cadence).
    private func checkDeadlines() {
        let now = Date()
        let warnWindow: TimeInterval = 600   // 10 min
        for t in tunnels {
            guard let endsAt = TunnelDeadlines.endsAt(t.name) else {
                warnedDeadlines.remove(t.name)
                continue
            }
            let remaining = endsAt.timeIntervalSince(now)
            if remaining <= 0 {
                // Allocation expired → prune the deadline so a later restart of
                // this tunnel (without re-picking a node) can't keep showing a
                // stale red "expired" countdown from the dead allocation.
                TunnelDeadlines.clear(t.name)
                warnedDeadlines.remove(t.name)
                continue
            }
            let on = (t.displayState == .alive || t.displayState == .starting)
            guard on else { warnedDeadlines.remove(t.name); continue }
            if remaining > warnWindow {
                warnedDeadlines.remove(t.name)        // re-arm
            } else if !warnedDeadlines.contains(t.name) {
                warnedDeadlines.insert(t.name)
                notchPresenter.show(
                    systemImage: "hourglass.bottomhalf.filled",
                    title: "\(t.name): ~\(max(1, Int(remaining / 60))) min left",
                    description: "Compute allocation expiring soon",
                    tint: .orange
                )
            }
        }
    }

    func reloadHostsOnly() async {
        do {
            self.hosts = try await client.listHosts()
            updateDockBadge()
            if connectionError != nil { connectionError = nil }
        } catch {
            // Event-driven refresh — swallow transient errors (don't flash a
            // banner). reloadAll's streak logic + the connection watcher own
            // the user-visible connection state.
            NSLog("[SSH2FA] reloadHostsOnly failed: \(error.localizedDescription)")
        }
    }

    func reloadTunnelsOnly() async {
        do {
            self.tunnels = try await client.listTunnels()
            updateDockBadge()
            // Clean stale dedup entries for tunnels that no longer exist
            // (renamed, deleted) so the dict doesn't grow forever AND so
            // a future tunnel re-using an old name gets a real first notch.
            let liveNames = Set(self.tunnels.map(\.name))
            self.lastNotchSignature = self.lastNotchSignature.filter { liveNames.contains($0.key) }
            checkDeadlines()   // event-driven path must also fire/prune expiry warnings
            if connectionError != nil { connectionError = nil }
        } catch {
            // Event-driven refresh — swallow transient errors (see reloadHostsOnly).
            NSLog("[SSH2FA] reloadTunnelsOnly failed: \(error.localizedDescription)")
        }
    }

    /// Set the Dock-tile badge to the # of alive tunnels (or to the # of
    /// failed things prefixed with "!"). Fires whenever state reloads.
    /// Also drives the persistent notch overlay (off by default).
    private func updateDockBadge() {
        var alive = 0
        var failed = 0
        for t in tunnels {
            switch t.displayState {
            case .alive: alive += 1
            case .failed, .portBusy: failed += 1
            default: break
            }
        }
        for h in hosts where h.displayState == .failed {
            failed += 1
        }
        let label: String?
        if failed > 0 { label = "!\(failed)" }
        else if alive > 0 { label = "\(alive)" }
        else { label = nil }
        NSApp.dockTile.badgeLabel = label
        // Refresh persistent notch (cheap — early-outs if signature unchanged).
        persistentNotch.update(from: self)
    }

    private func startEventTask() {
        eventTask?.cancel()
        eventTask = Task { [weak self] in
            // Fresh per-subscriber stream each (re)subscription — cancelling
            // the old task finished its stream, so re-iterating a SHARED
            // stream after the first reconnect silently dropped every event
            // (the app degraded to 5s polling without telling anyone).
            guard let client = self?.client else { return }
            let stream = await client.eventStream()
            for await event in stream {
                guard let self else { return }
                await self.apply(event: event)
            }
        }
    }

    private func startPollFallback() {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000) // 5s safety net
                guard let self else { return }
                // ALWAYS poll. Gating on `connectionError == nil` permanently
                // disabled the poll once reloadAll itself set the "slow to
                // respond — retrying…" banner (which nothing then retried) or
                // any per-action error set connectionError: the banner claimed
                // retrying while the app sat frozen. The socket-level
                // disconnect case is handled separately (reconnectWithBackoff)
                // and reloadAll fails fast + cheap while down.
                await self.reloadAll()
            }
        }
    }

    private func apply(event: DaemonEvent) async {
        switch event {
        case .hostChanged:
            // Daemon's host event doesn't carry the full snapshot, so we
            // refetch hosts only (NOT tunnels — that used to thrash the UI
            // on every host heartbeat tick).
            await reloadHostsOnly()
        case .tunnelChanged(let name, let status, let lastMsg, _):
            let prev = tunnels.first(where: { $0.name == name })
            let wasAlive: Bool = (prev?.displayState == Tunnel.DisplayState.alive)
            // Only reload tunnels (not hosts) on a tunnel event.
            await reloadTunnelsOnly()
            maybeShowNotch(name: name, status: status, lastMsg: lastMsg)
            if status == "alive" && !wasAlive {
                if let t = tunnels.first(where: { $0.name == name }) {
                    maybeAutoOpenBrowser(for: t)
                }
            }
            // Hand-off: post a macOS notification on hard failures with
            // action buttons (Restart / Show Activity) so user can react
            // without switching back to the app.
            if status == "failed" || status == "stale" {
                MacNotifications.postTunnelFailed(
                    name: name,
                    body: lastMsg.isEmpty ? "see app for details" : lastMsg
                )
            }
        case .notification(let severity, let title, let message):
            notchPresenter.show(
                systemImage: severity == "error" ? "exclamationmark.octagon.fill"
                          : severity == "warning" ? "exclamationmark.triangle.fill"
                          : "info.circle.fill",
                title: title,
                description: message,
                tint: severity == "error" ? .red : severity == "warning" ? .orange : .blue
            )
        case .unknown:
            break
        }
    }

    /// Honour the "Open URL in browser on tunnel up" setting. Fires from
    /// apply(event:) once per idle/starting → alive transition.
    /// Uses browserURL so per-tunnel url_path suffix (e.g. jupyter token)
    /// is appended automatically.
    private func maybeAutoOpenBrowser(for t: Tunnel) {
        guard UserDefaults.standard.bool(forKey: "auto2fa.autoOpenBrowser") else { return }
        if let url = URL(string: t.browserURL) {
            NSWorkspace.shared.open(url)
        }
    }

    /// Names+statuses we last toasted, to avoid spamming the notch when a
    /// daemon-side change-detector mistakenly fires the same status over and
    /// over. (Belt-and-suspenders — the daemon's _tunnel_change_key handles
    /// the real fix, this just prevents any future regression from drowning
    /// the user in notches.)
    private var lastNotchSignature: [String: String] = [:]

    /// Dedup key for a tunnel's notch. The daemon AUTO-STOPS a tunnel (node
    /// gone from squeue, or repeated recovery failures) by prefixing `last_msg`
    /// with "Auto-stopped:" and clearing wants_alive. That deserves its own
    /// notice distinct from a transient failure — and the signature must encode
    /// it so a transient-failed → auto-stopped transition (same status string)
    /// still fires exactly once.
    private func notchSignature(status: String, lastMsg: String) -> String {
        lastMsg.hasPrefix("Auto-stopped:") ? "autostop:\(status)" : status
    }

    private func maybeShowNotch(name: String, status: String, lastMsg: String) {
        if UserDefaults.standard.object(forKey: "auto2fa.notch.enabled") != nil,
           UserDefaults.standard.bool(forKey: "auto2fa.notch.enabled") == false {
            return
        }
        // Dedup: if the last notch we showed for this tunnel had the same
        // signature, skip. This makes "Connected" fire only on a real
        // idle/starting → alive transition, never on repeat snapshots.
        let sig = notchSignature(status: status, lastMsg: lastMsg)
        if lastNotchSignature[name] == sig { return }
        lastNotchSignature[name] = sig
        // Auto-stop (node gone / repeated failures): a distinct, clear notice —
        // the tunnel gave up and won't keep retrying.
        if lastMsg.hasPrefix("Auto-stopped:") {
            let reason = String(lastMsg.dropFirst("Auto-stopped: ".count))
            notchPresenter.show(
                systemImage: "stop.circle.fill",
                title: "Tunnel stopped",
                description: "\(name): \(reason)",
                tint: .orange
            )
            return
        }
        switch status {
        case "alive":
            notchPresenter.show(
                systemImage: "bolt.fill",
                title: "Connected",
                description: name,
                tint: .green
            )
        case "failed", "stale":
            notchPresenter.show(
                systemImage: "exclamationmark.triangle.fill",
                title: status == "failed" ? "Disconnected" : "Node ended",
                description: "\(name): \(lastMsg)",
                tint: .red
            )
        case "starting":
            notchPresenter.show(
                systemImage: "arrow.triangle.2.circlepath",
                title: "Connecting…",
                description: name,
                tint: .yellow
            )
        default:
            break
        }
    }

    // MARK: - User actions (thin wrappers that report errors via connectionError)

    /// Surface a per-action failure as a red toast. NOT via `connectionError`:
    /// the follow-up `reloadAll()` clears that banner on its next success
    /// (within ~1 s), so action errors written there vanished before the user
    /// could read them — a daemon rejection like "port busy" was effectively
    /// invisible.
    func showActionError(_ message: String) {
        notchPresenter.show(
            systemImage: "exclamationmark.triangle.fill",
            title: "Action failed",
            description: message,
            tint: .red
        )
    }
    func showActionError(_ error: Error) {
        showActionError(error.localizedDescription)
    }

    func toggleHost(_ host: SSHHost) async {
        inFlightHosts.insert(host.host)
        defer { inFlightHosts.remove(host.host) }
        // Immediate notch so the user sees their click landed.
        notchPresenter.show(
            // Verb is based on whether the host is currently ON (active), not on
            // whether it's fully connected — so stopping a host stuck
            // reconnecting correctly shows "Stopping", not "Starting".
            systemImage: host.active ? "stop.fill" : "arrow.triangle.2.circlepath",
            title: host.active ? "Stopping" : "Starting",
            description: host.host,
            tint: .yellow
        )
        do { try await client.toggleHost(host.host) }
        catch { showActionError(error) }
        await reloadAll()
    }

    /// Force a fresh connection attempt on a host. A FAILED host is still
    /// `active` (the daemon keeps retrying it in backoff), so a single
    /// `toggleHost` would just STOP it — the "Retry" affordance must instead
    /// stop-then-start to actually reconnect (and reset the circuit breaker).
    /// The reload between gives the daemon's synchronous `active=false` time to
    /// settle before the re-activation (the rapid OFF→ON race is daemon-guarded).
    func retryHost(_ host: SSHHost) async {
        inFlightHosts.insert(host.host)
        defer { inFlightHosts.remove(host.host) }
        notchPresenter.show(
            systemImage: "arrow.triangle.2.circlepath",
            title: "Retrying",
            description: host.host,
            tint: .yellow
        )
        do {
            if host.active {
                try await client.toggleHost(host.host)   // active(failed) → deactivate
                await reloadAll()
                // Re-read live state: only re-activate if it's ACTUALLY inactive
                // now. If the daemon's own retry brought it back active in the
                // gap, a blind second toggle would deactivate it again ("Retry"
                // → stop). If it's already active, the goal is met — leave it.
                let nowActive = hosts.first(where: { $0.host == host.host })?.active ?? false
                if !nowActive {
                    try await client.toggleHost(host.host)   // idle → activate (fresh connect)
                }
            } else {
                try await client.toggleHost(host.host)   // already idle → activate
            }
        } catch { showActionError(error) }
        await reloadAll()
    }

    func toggleTunnel(_ tunnel: Tunnel) async {
        inFlightTunnels.insert(tunnel.name)
        defer { inFlightTunnels.remove(tunnel.name) }
        notchPresenter.show(
            systemImage: (tunnel.displayState == .alive || tunnel.displayState == .starting) ? "stop.fill" : "arrow.triangle.2.circlepath",
            title: (tunnel.displayState == .alive || tunnel.displayState == .starting) ? "Stopping" : "Starting",
            description: tunnel.name,
            tint: .yellow
        )
        do { try await client.toggleTunnel(tunnel.name) }
        catch { showActionError(error) }
        await reloadAll()
    }

    func deleteTunnel(_ tunnel: Tunnel) async {
        inFlightTunnels.insert(tunnel.name)
        defer { inFlightTunnels.remove(tunnel.name) }
        var deleted = true
        do { try await client.removeTunnel(tunnel.name) }
        catch {
            deleted = false
            showActionError(error)
        }
        // Drop any compute-allocation countdown so a future tunnel that reuses
        // this name doesn't inherit the deleted one's deadline.
        if deleted { TunnelDeadlines.clear(tunnel.name) }
        await reloadAll()
        // Offer Undo ONLY if the delete actually happened — otherwise the
        // snackbar said "Deleted" for a tunnel that's still there, and Undo
        // then failed with a duplicate-name error.
        guard deleted else { return }
        // Stash a snapshot so the snackbar can offer Undo for ~8s.
        undoableDelete = tunnel
        undoExpireTask?.cancel()
        undoExpireTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 8_000_000_000)
            guard let self else { return }
            await MainActor.run {
                if self.undoableDelete?.name == tunnel.name {
                    self.undoableDelete = nil
                }
            }
        }
    }

    /// Re-create a tunnel from a snapshot. Used by the Undo snackbar after
    /// a delete. We re-issue the addTunnel + restore the persistent fields
    /// (auto_start, post_connect_cmd, tags, jump_candidates, last_node).
    ///
    /// IMPORTANT: tunnel_set_node has a side effect of STARTING the tunnel
    /// on the daemon side. So we only call it if the tunnel was alive at
    /// delete time — restoring an idle tunnel that just happens to have a
    /// `lastNode` from a previous run would otherwise unexpectedly start
    /// it. If you want a faithful restore that doesn't kick the tunnel,
    /// the daemon would need a `set_node_no_start` flavor; for now the
    /// approximation is "was alive → keep it alive; was idle → leave idle".
    func undoDelete() async {
        guard let t = undoableDelete else { return }
        undoableDelete = nil
        undoExpireTask?.cancel()
        do {
            // remote_port MUST be passed: addTunnel defaults remote=local on
            // the daemon side, so undoing "9999 → 8888" used to recreate
            // "9999 → 9999" — silent config corruption.
            _ = try await client.addTunnel(name: t.name, localPort: t.localPort,
                                           remotePort: t.remotePort,
                                           directHost: t.directHost)
            if t.autoStart {
                try? await client.setTunnelAutostart(t.name, value: true)
            }
            if !t.tags.isEmpty {
                try? await client.setTunnelTags(t.name, tags: t.tags)
            }
            if let cmd = t.postConnectCmd, !cmd.isEmpty {
                try? await client.setTunnelPostConnect(t.name, cmd: cmd)
            }
            if let jc = t.jumpCandidates {
                try? await client.setTunnelJumpCandidates(t.name, candidates: jc)
            }
            if let up = t.urlPath, !up.isEmpty {
                try? await client.setTunnelUrlPath(t.name, path: up)
            }
            // Only restart if it was alive at delete time; idle tunnels stay
            // idle. Direct tunnels have no node — start them via toggle so undo
            // is faithful; compute tunnels restart by re-setting the node.
            if t.displayState == .alive {
                if t.isDirect {
                    try? await client.toggleTunnel(t.name)
                } else if let node = t.lastNode, !node.isEmpty {
                    try? await client.setTunnelNode(t.name, node: node,
                                                    user: t.lastUser ?? "")
                }
            }
            await reloadTunnelsOnly()
            FriendlyText.haptic()
            notchPresenter.show(
                systemImage: "arrow.uturn.backward",
                title: "Restored",
                description: t.name,
                tint: .green
            )
        } catch {
            showActionError("Couldn't restore: \(error.localizedDescription)")
        }
    }

    /// Clone an existing tunnel: same node/jump/tags/post-connect, next
    /// free port, name = `<original>-copy[-N]`. Returns the new name
    /// (or nil on failure).
    @discardableResult
    func cloneTunnel(_ t: Tunnel) async -> String? {
        let newName = nextCloneName(for: t.name)
        do {
            let newPort = try await client.suggestPort(base: t.localPort + 1)
            // Keep the ORIGINAL remote port: a clone of "8888 → 6006" must
            // forward to 6006, not to its own new local port.
            _ = try await client.addTunnel(name: newName, localPort: newPort,
                                           remotePort: t.remotePort,
                                           directHost: t.directHost)
            if !t.tags.isEmpty {
                try? await client.setTunnelTags(newName, tags: t.tags)
            }
            if let cmd = t.postConnectCmd, !cmd.isEmpty {
                try? await client.setTunnelPostConnect(newName, cmd: cmd)
            }
            if let jc = t.jumpCandidates {
                try? await client.setTunnelJumpCandidates(newName, candidates: jc)
            }
            if let up = t.urlPath, !up.isEmpty {
                try? await client.setTunnelUrlPath(newName, path: up)
            }
            // Only start the clone if the original is live — cloning an IDLE
            // tunnel shouldn't auto-SSH. Direct clones start via toggle (no
            // node); compute clones start by setting the node. Mirrors undoDelete.
            if t.displayState == .alive {
                if t.isDirect {
                    try? await client.toggleTunnel(newName)
                } else if let node = t.lastNode, !node.isEmpty {
                    try? await client.setTunnelNode(newName, node: node,
                                                    user: t.lastUser ?? "")
                }
            }
            await reloadTunnelsOnly()
            FriendlyText.haptic()
            notchPresenter.show(
                systemImage: "doc.on.doc.fill",
                title: "Cloned",
                description: "\(t.name) → \(newName)",
                tint: .blue
            )
            return newName
        } catch {
            // showActionError, not connectionError: the next successful 5s
            // poll wiped the banner before the user could read it.
            showActionError("Clone failed: \(error.localizedDescription)")
            return nil
        }
    }

    private func nextCloneName(for base: String) -> String {
        let stem = base.hasSuffix("-copy") ? String(base.dropLast(5)) : base
        let names = Set(tunnels.map(\.name))
        var candidate = "\(stem)-copy"
        var i = 2
        while names.contains(candidate) {
            candidate = "\(stem)-copy-\(i)"
            i += 1
        }
        return candidate
    }

    func rotateHost(_ host: SSHHost) async {
        inFlightHosts.insert(host.host)
        defer { inFlightHosts.remove(host.host) }
        do { try await client.rotateHost(host.host) }
        catch { showActionError(error) }
        await reloadAll()
    }

    func toggleMount(_ host: SSHHost) async {
        inFlightHosts.insert(host.host)
        defer { inFlightHosts.remove(host.host) }
        do { try await client.toggleMount(host.host) }
        catch { showActionError(error) }
        await reloadAll()
    }

    /// Live TOTP code for a host (6-digit, never the secret). Thin passthrough
    /// to the backend client — the TOTP chip calls this and handles failure
    /// itself (it shows a muted placeholder rather than a global banner), so
    /// we deliberately rethrow instead of swallowing into connectionError.
    func hostTOTP(_ host: String) async throws -> BackendClient.TOTPCode {
        try await client.hostTOTP(host)
    }

    // MARK: - Sheet helpers

    func presentNewTunnel() { activeSheet = .newTunnel }
    func presentNodePicker(for tunnel: Tunnel) { activeSheet = .nodePicker(tunnelName: tunnel.name) }
    func presentCustomNode(for tunnelName: String) { activeSheet = .customNode(tunnelName: tunnelName) }
    func presentConfirmDelete(for tunnel: Tunnel) { activeSheet = .confirmDelete(tunnelName: tunnel.name) }
    func presentAddHost(prefillAlias: String? = nil) { activeSheet = .addHost(prefillAlias: prefillAlias) }
    func presentImport() { refreshConfigCache(); activeSheet = .importHosts }
    func dismissSheet() { activeSheet = nil }

    /// Re-parse ~/.ssh/config into the in-memory cache. Cheap (small file) and
    /// the single disk read for everything config-derived below.
    func refreshConfigCache() {
        let dir = SSHPaths.sshDir()
        // Follow Include directives so split configs (`Include config.d/*`) are
        // discoverable too — not just top-level Host blocks.
        parsedConfig = SSHConfigParser.parseConfig(at: SSHPaths.configFile(dir: dir), configDir: dir)
    }

    /// Hosts parsed from ~/.ssh/config (concrete Host blocks), from the cache.
    var configHosts: [ConfigHost] { parsedConfig.hosts }

    /// Config hosts not yet registered — fuel for the import sheet.
    var importableHosts: [ConfigHost] {
        SSHSyncDiff.importable(configHosts: parsedConfig.hosts, registered: hosts.map { $0.host })
    }

    /// Registered hosts that genuinely can't be reached from the config — kept
    /// conservative (quiet for wildcard-covered hosts, and when the view is
    /// incomplete: Match blocks or an unresolved Include) so it doesn't
    /// false-alarm on advanced configs.
    var unreachableRegisteredHosts: [String] {
        SSHSyncDiff.unreachable(registered: hosts.map { $0.host },
                                configAliases: parsedConfig.hosts.map { $0.alias },
                                patterns: parsedConfig.patterns,
                                configIncompleteView: parsedConfig.incompleteView)
    }

    /// Regenerate ssh2fa.conf + the daemon wrapper from the live host list and
    /// the sidecar. ALWAYS runs (the daemon resolves via these files) — the
    /// terminal-reuse Include into ~/.ssh/config stays a separate opt-in.
    /// Safe on every reload (writes skip unchanged content).
    func syncManagedSSHConfig() {
        let dir = SSHPaths.sshDir()
        let sidecar = ManagedHostStore.load(from: managedHostsURL)
        // Union of registered hosts + sidecar-only aliases, so a guided host
        // pending its test-login (in the sidecar but not yet registered) still
        // gets a Host block that `ssh -F` can resolve during the test.
        let managed = SSHConfigManager.mergedManagedHosts(registered: hosts.map { $0.host },
                                                          sidecar: sidecar)
        _ = try? SSHConfigManager.writeManagedConf(hosts: managed, dir: dir)
        _ = try? SSHConfigManager.writeDaemonWrapper(dir: dir)
    }

    /// Sidecar location: ~/.ssh2fa/managed_hosts.json.
    var managedHostsURL: URL {
        URL(fileURLWithPath: NSHomeDirectory())
            .appendingPathComponent(".ssh2fa")
            .appendingPathComponent("managed_hosts.json")
    }

    /// Create a tunnel. Returns nil on success, or a user-displayable error
    /// message on failure (so the sheet can show it inline rather than
    /// duplicating it as a global banner).
    func createTunnel(name: String, localPort: Int, remotePort: Int? = nil,
                      autoStart: Bool = false, directHost: String? = nil) async -> String? {
        inFlightTunnels.insert(name)
        defer { inFlightTunnels.remove(name) }
        do {
            _ = try await client.addTunnel(name: name, localPort: localPort,
                                           remotePort: remotePort, directHost: directHost)
            if autoStart {
                try? await client.setTunnelAutostart(name, value: true)
            }
            dismissSheet()
            await reloadAll()
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }

    /// Flip a tunnel's auto-start flag. Persistent across daemon restarts.
    func setTunnelAutostart(_ tunnel: Tunnel, value: Bool) async {
        do { try await client.setTunnelAutostart(tunnel.name, value: value) }
        catch { showActionError(error) }
        await reloadAll()
    }

    /// Pin (or unpin) the tunnel's jump host. nil = auto pick any ready host;
    /// non-nil = priority-ordered list, daemon takes the first ready entry.
    /// If the tunnel is currently alive the daemon restarts it through the
    /// new candidates so the change takes effect immediately.
    func setJumpCandidates(for tunnel: Tunnel, candidates: [String]?) async {
        inFlightTunnels.insert(tunnel.name)
        defer { inFlightTunnels.remove(tunnel.name) }
        do { try await client.setTunnelJumpCandidates(tunnel.name, candidates: candidates) }
        catch { showActionError(error) }
        await reloadAll()
    }

    func setPostConnect(for tunnel: Tunnel, cmd: String?) async {
        do { try await client.setTunnelPostConnect(tunnel.name, cmd: cmd) }
        catch { showActionError(error) }
        await reloadAll()
    }

    func setTags(for tunnel: Tunnel, tags: [String]) async {
        do { try await client.setTunnelTags(tunnel.name, tags: tags) }
        catch { showActionError(error) }
        await reloadTunnelsOnly()
    }

    func setUrlPath(for tunnel: Tunnel, path: String?) async {
        do { try await client.setTunnelUrlPath(tunnel.name, path: path) }
        catch { showActionError(error) }
        await reloadTunnelsOnly()
    }

    /// Rename a tunnel. Returns nil on success or an error message.
    @discardableResult
    func renameTunnel(_ tunnel: Tunnel, to newName: String) async -> String? {
        let new = newName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !new.isEmpty, new != tunnel.name else { return nil }
        inFlightTunnels.insert(tunnel.name)
        defer { inFlightTunnels.remove(tunnel.name) }
        do {
            try await client.renameTunnel(old: tunnel.name, new: new)
            await reloadTunnelsOnly()
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }

    /// Apply imported tunnel definitions. Each one is added via the same
    /// add+configure dance as Undo. Existing names are skipped (renaming
    /// is the user's job — silent overwrite would surprise them).
    /// Returns counts so caller can toast a summary.
    func importTunnels(_ imported: [TunnelExportImport.ExportedTunnel])
        async -> (added: Int, skipped: Int, failed: Int)
    {
        var added = 0, skipped = 0, failed = 0
        let existing = Set(tunnels.map(\.name))
        for t in imported {
            if existing.contains(t.name) { skipped += 1; continue }
            do {
                // remote_port is in the export file and MUST be passed —
                // otherwise "8888 → 6006" imports as "8888 → 8888" and the
                // forward silently targets the wrong remote port.
                _ = try await client.addTunnel(name: t.name, localPort: t.local_port,
                                               remotePort: t.remote_port,
                                               directHost: t.direct_host)
                if t.auto_start {
                    try? await client.setTunnelAutostart(t.name, value: true)
                }
                if !t.tags.isEmpty {
                    try? await client.setTunnelTags(t.name, tags: t.tags)
                }
                if let cmd = t.post_connect_cmd, !cmd.isEmpty {
                    try? await client.setTunnelPostConnect(t.name, cmd: cmd)
                }
                if let jc = t.jump_candidates {
                    try? await client.setTunnelJumpCandidates(t.name, candidates: jc)
                }
                if let up = t.url_path, !up.isEmpty {
                    try? await client.setTunnelUrlPath(t.name, path: up)
                }
                if let node = t.last_node, !node.isEmpty {
                    // start:false — restoring a backup records each tunnel's
                    // node WITHOUT firing an immediate SSH start at a
                    // possibly-dead SLURM node (auto_start tunnels still come
                    // up on the next daemon boot). Importing 10 tunnels used
                    // to launch 10 starts + a toast storm.
                    try? await client.setTunnelNode(t.name, node: node,
                                                    user: t.last_user ?? "",
                                                    start: false)
                }
                added += 1
            } catch {
                failed += 1
            }
        }
        await reloadTunnelsOnly()
        notchPresenter.show(
            systemImage: "square.and.arrow.down",
            title: "Imported \(added)",
            description: "\(skipped) skipped, \(failed) failed",
            tint: failed > 0 ? .orange : .green
        )
        return (added, skipped, failed)
    }

    /// True while a tunnels_batch RPC is in flight — a second click on the
    /// batch Start/Stop buttons during a slow batch (daemon timeout 30s) used
    /// to dispatch a second overlapping batch.
    @Published var batchInFlight = false

    /// Best-effort batch start/stop. Toasts a single summary at the end.
    func batchTunnels(action: String, names: [String]) async {
        guard !batchInFlight else { return }
        batchInFlight = true
        defer { batchInFlight = false }
        do {
            let results = try await client.batchTunnels(action: action, names: names)
            let okCount = results.filter { $0.ok }.count
            notchPresenter.show(
                systemImage: action == "start" ? "play.fill" : "stop.fill",
                title: "\(okCount)/\(results.count) \(action)ed",
                description: names.joined(separator: ", "),
                tint: okCount == results.count ? .green : .orange
            )
        } catch { showActionError(error) }
        await reloadTunnelsOnly()
    }

    /// Nuclear reset — stop everything, rebuild every master. Use sparingly.
    func resetAll() async {
        do {
            let r = try await client.resetAll()
            notchPresenter.show(
                systemImage: "exclamationmark.arrow.circlepath",
                title: "Reset complete",
                description: "\(r.tunnelsStopped) tunnels stopped, \(r.mastersRebuilt) masters rebuilding",
                tint: .orange
            )
        } catch { showActionError(error) }
        await reloadAll()
    }

    /// Add a new host via daemon. Returns nil on success, error message on failure.
    @discardableResult
    func addHost(host: String, password: String, otpauthURL: String,
                 autoConnect: Bool) async -> String? {
        do {
            _ = try await client.addHost(host: host, password: password,
                                         otpauthURL: otpauthURL,
                                         autoConnect: autoConnect)
            await reloadAll()
            WarmReuseConsent.offerIfNeeded(currentAliases: hosts.map { $0.host })
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }

    /// Guided add: persist the connection fields, regenerate the managed ssh
    /// config + wrapper, THEN register credentials (so the test-login + master
    /// resolve the new host via `ssh -F`). Returns nil on success or an error.
    @discardableResult
    func addManagedHost(alias: String, hostName: String, user: String, port: Int,
                        password: String, otpauthURL: String, autoConnect: Bool) async -> String? {
        do {
            try ManagedHostStore.upsert(
                ManagedHostConn(alias: alias, hostName: hostName, user: user, port: port),
                in: managedHostsURL)
        } catch {
            return "Couldn't save connection settings: \(error.localizedDescription)"
        }
        syncManagedSSHConfig()           // writes ssh2fa.conf + wrapper before login
        return await addHost(host: alias, password: password,
                             otpauthURL: otpauthURL, autoConnect: autoConnect)
    }

    /// Set a node on a tunnel (also kicks off start via set_node on the
    /// daemon side). Returns nil on success or an error message on failure.
    @discardableResult
    func pickNode(for tunnelName: String, node: String, user: String) async -> String? {
        inFlightTunnels.insert(tunnelName)
        defer { inFlightTunnels.remove(tunnelName) }
        do {
            try await client.setTunnelNode(tunnelName, node: node, user: user)
            RecentNodes.record(node)
            // Drop any prior compute-allocation countdown — the node just
            // changed. NodePicker re-sets a fresh deadline after this returns
            // (it has the SqueueJob's TIME_LEFT); CustomNode has none, so the
            // tunnel correctly ends up with no stale countdown.
            TunnelDeadlines.clear(tunnelName)
            dismissSheet()
            await reloadAll()
            return nil
        } catch {
            return (error as? BackendClient.ClientError)?.errorDescription
                ?? error.localizedDescription
        }
    }
}
