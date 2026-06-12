import SwiftUI
import AppKit
import UserNotifications

@main
struct Auto2FAApp: App {
    @StateObject private var appState = AppState()
    @StateObject private var menuBar = MenuBarController()
    @StateObject private var biometricLock = BiometricLock()
    // Kept as instance vars so they aren't deallocated + unsubscribed.
    @State private var sleepWakeMonitor: SleepWakeMonitor?
    @State private var networkMonitor: NetworkMonitor?
    @State private var preferenceSync: PreferenceSync?

    var body: some Scene {
        WindowGroup("SSH2FA") {
            LockGate {
                ContentView()
                    .environmentObject(appState)
            }
            .environmentObject(biometricLock)
            // Clear window background — the desktop wallpaper shows through;
            // content carries its own real Liquid Glass cards (no gray material).
            .containerBackground(.clear, for: .window)
            .onAppear {
                SingleInstance.enforceOrExit()
                installMenuBarOnce()
                installSleepWakeMonitor()
                installNetworkMonitor()
                installNotificationHandling()
                installPreferenceSync()
            }
            .task {
                // Spawn daemon first (or detect existing one), THEN run
                // AppState.bootstrap. Doing this serially in a single
                // .task avoids the race where ContentView's own .task
                // would fire bootstrap before we've started the daemon.
                //
                // Honor the spawnDaemonOnLaunch user preference — if
                // off, we just try to connect to an externally-managed
                // daemon (LaunchAgent, manual launch, etc).
                let spawnAllowed = UserDefaults.standard
                    .object(forKey: SettingsKey.spawnDaemonOnLaunch) as? Bool ?? true
                if spawnAllowed {
                    // First-run / post-update: install the bundled daemon to
                    // ~/.ssh2fa + register the LaunchAgent (no-op in a dev
                    // build or when already installed). This is what makes
                    // a downloaded .app self-contained — launchd then keeps
                    // the daemon alive across reboots; ensureRunning below
                    // just connects (or direct-spawns as a fallback).
                    DaemonProcess.shared.installBundledDaemonIfNeeded()
                    let result = await DaemonProcess.shared.ensureRunning()
                    switch result {
                    case .alreadyRunning:
                        NSLog("[SSH2FA] daemon was already running")
                    case .spawned(let pid):
                        NSLog("[SSH2FA] spawned daemon, PID=\(pid)")
                    case .failed(let reason):
                        // Surface the reason but DON'T return — fall
                        // through to bootstrap() so the connection watcher
                        // + poll fallback start and recover once a daemon
                        // appears (launchd may bring one up seconds later).
                        // The old `return` was a permanent dead end.
                        appState.connectionError = reason
                    }
                } else {
                    NSLog("[SSH2FA] spawnDaemonOnLaunch=off; assuming external daemon")
                }
                await appState.bootstrap()
            }
        }
        .defaultSize(width: 820, height: 540)
        .windowToolbarStyle(.unified)
        .commands {
            // Replace the standard File → New so ⌘N opens our New Tunnel sheet.
            CommandGroup(replacing: .newItem) {
                Button("New Tunnel…") {
                    appState.presentNewTunnel()
                }
                .keyboardShortcut("n", modifiers: [.command])
                Button("Command Palette…") {
                    NotificationCenter.default.post(name: .a2fShowPalette, object: nil)
                }
                .keyboardShortcut("p", modifiers: [.command, .shift])
            }
            CommandGroup(after: .saveItem) {
                Button("Export Tunnels…") {
                    // Surface a write failure (disk full / read-only volume) —
                    // it was silently swallowed, so a "backup" could fail with
                    // zero feedback. nil = success, "cancelled" = user cancelled.
                    if let err = TunnelExportImport.exportToFile(appState.tunnels),
                       err != "cancelled" {
                        appState.showActionError("Export failed: \(err)")
                    }
                }
                .keyboardShortcut("e", modifiers: [.command, .shift])
                Button("Import Tunnels…") {
                    let (imported, err) = TunnelExportImport.importFromFile()
                    if let imported, !imported.isEmpty {
                        Task { _ = await appState.importTunnels(imported) }
                    } else if let err, err != "cancelled" {
                        // Action toast — connectionError gets wiped by the
                        // next successful poll before it can be read.
                        appState.showActionError(err)
                    }
                }
            }
            CommandGroup(replacing: .help) {
                Link("SSH2FA on GitHub",
                     destination: URL(string: "https://github.com/gasvn/ssh2fa")!)
            }
        }

        // ⌘, opens this automatically.
        Settings {
            SettingsView()
        }

        // A separate WindowGroup for the log viewer. SwiftUI auto-adds a
        // "New SSH2FA Logs Window" item to the Window menu since this is a
        // second WindowGroup. The menu-bar status item also offers a quick
        // way to open it via MenuBarController.
        WindowGroup("SSH2FA Logs", id: "logs") {
            LockGate {
                LogViewerView()
                    .environmentObject(appState)
            }
            .environmentObject(biometricLock)
        }
        .defaultSize(width: 900, height: 600)
    }

    /// Install the menu-bar status item exactly once per app launch. Done
    /// from .onAppear of the main window's content so the NSWindow exists
    /// by the time we look for it.
    private func installMenuBarOnce() {
        guard !menuBar.isInstalled else { return }
        // The main window is whichever NSWindow this WindowGroup vended. Pick
        // the most recently key window with a non-nil titlebar — that's our
        // main window, not the menu-bar overflow window or a settings panel.
        let window = NSApp.windows.first { $0.isVisible && $0.title == "SSH2FA" }
            ?? NSApp.windows.first
        menuBar.install(appState: appState, window: window)
    }

    /// On Mac wake-from-sleep, every SSH master's TCP is dead. Tell the
    /// daemon to rebuild masters and restart previously-alive tunnels. Also
    /// surface a small notch toast so the user sees we're doing something.
    /// Also hook applicationWillTerminate so we can shut down the daemon we
    /// spawned (using NotificationCenter avoids @NSApplicationDelegateAdaptor,
    /// which in some macOS releases prevents the SwiftUI window from showing).
    /// Fire wake_recover whenever the network interface changes (Wi-Fi
    /// switch, VPN up/down, ethernet plug). Sibling to SleepWakeMonitor —
    /// SSH masters die silently on network switch too, not just sleep.
    private func installNetworkMonitor() {
        guard networkMonitor == nil else { return }
        let mon = NetworkMonitor {
            // Respect the same setting that gates wake recovery.
            let recover = UserDefaults.standard
                .object(forKey: SettingsKey.autoRecoverOnWake) as? Bool ?? true
            guard recover else { return }
            Task { @MainActor in
                // Toast only when the daemon actually RAN a recovery pass —
                // a single wake fires both monitors, and the second call is
                // coalesced (daemon- or client-side). Claiming "Probing
                // tunnels…" for a no-op was misleading.
                let ran = (try? await appState.client.wakeRecover()) ?? false
                if ran {
                    appState.notchPresenter.show(
                        systemImage: "network",
                        title: "Network changed",
                        description: "Probing tunnels…",
                        tint: .yellow
                    )
                }
                await appState.reloadAll()
            }
        }
        mon.start()
        networkMonitor = mon
    }

    /// Start free iCloud-Drive preference sync (no-op unless the user opted in
    /// and is signed into iCloud Drive).
    private func installPreferenceSync() {
        guard preferenceSync == nil else { return }
        let sync = PreferenceSync()
        sync.start()
        preferenceSync = sync
    }

    /// Hook the notification action buttons (Restart / Show Activity) up
    /// to AppState so clicking them does the right thing.
    private func installNotificationHandling() {
        UNUserNotificationCenter.current().delegate = NotificationDelegate.shared
        NotificationDelegate.shared.appState = appState
        MacNotifications.registerCategories()
    }

    private func installSleepWakeMonitor() {
        guard sleepWakeMonitor == nil else { return }
        let monitor = SleepWakeMonitor(
            onSleep: {
                appState.notchPresenter.show(
                    systemImage: "moon.zzz.fill",
                    title: "Sleeping",
                    description: "tunnels will auto-recover on wake",
                    tint: .secondary
                )
            },
            onWake: {
                // Honor the autoRecoverOnWake setting. Default is true.
                let recover = UserDefaults.standard
                    .object(forKey: SettingsKey.autoRecoverOnWake) as? Bool ?? true
                guard recover else {
                    NSLog("[SSH2FA] wake observed; autoRecoverOnWake=off, skipping")
                    return
                }
                appState.notchPresenter.show(
                    systemImage: "arrow.triangle.2.circlepath",
                    title: "Recovering tunnels…",
                    description: "rebuilding SSH masters",
                    tint: .yellow
                )
                Task {
                    do {
                        try await appState.client.wakeRecover()
                        NSLog("[SSH2FA] wake_recover dispatched")
                    } catch {
                        NSLog("[SSH2FA] wake_recover failed: \(error.localizedDescription)")
                    }
                    await appState.reloadAll()
                }
            }
        )
        monitor.start()
        sleepWakeMonitor = monitor

        // Shut down the daemon we spawned when the app quits.
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { _ in
            DaemonProcess.shared.terminateOwnedDaemon()
        }
    }
}
