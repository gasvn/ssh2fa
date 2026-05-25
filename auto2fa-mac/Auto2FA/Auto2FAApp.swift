import SwiftUI
import AppKit
import UserNotifications

@main
struct Auto2FAApp: App {
    @StateObject private var appState = AppState()
    @StateObject private var menuBar = MenuBarController()
    // Kept as instance vars so they aren't deallocated + unsubscribed.
    @State private var sleepWakeMonitor: SleepWakeMonitor?
    @State private var networkMonitor: NetworkMonitor?

    var body: some Scene {
        WindowGroup("Auto2FA") {
            ContentView()
                .environmentObject(appState)
                .onAppear {
                    SingleInstance.enforceOrExit()
                    installMenuBarOnce()
                    installSleepWakeMonitor()
                    installNetworkMonitor()
                    installNotificationHandling()
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
                        let result = await DaemonProcess.shared.ensureRunning()
                        switch result {
                        case .alreadyRunning:
                            NSLog("[Auto2FA] daemon was already running")
                        case .spawned(let pid):
                            NSLog("[Auto2FA] spawned daemon, PID=\(pid)")
                        case .failed(let reason):
                            appState.connectionError = reason
                            return
                        }
                    } else {
                        NSLog("[Auto2FA] spawnDaemonOnLaunch=off; assuming external daemon")
                    }
                    await appState.bootstrap()
                }
        }
        .defaultSize(width: 820, height: 540)
        .windowToolbarStyle(.unifiedCompact)
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
                    _ = TunnelExportImport.exportToFile(appState.tunnels)
                }
                .keyboardShortcut("e", modifiers: [.command, .shift])
                Button("Import Tunnels…") {
                    let (imported, err) = TunnelExportImport.importFromFile()
                    if let imported, !imported.isEmpty {
                        Task { _ = await appState.importTunnels(imported) }
                    } else if let err, err != "cancelled" {
                        appState.connectionError = err
                    }
                }
            }
            CommandGroup(replacing: .help) {
                Link("Auto2FA on GitHub",
                     destination: URL(string: "https://github.com/gasvn/auto2fa")!)
            }
        }

        // ⌘, opens this automatically.
        Settings {
            SettingsView()
        }

        // A separate WindowGroup for the log viewer. SwiftUI auto-adds a
        // "New Auto2FA Logs Window" item to the Window menu since this is a
        // second WindowGroup. The menu-bar status item also offers a quick
        // way to open it via MenuBarController.
        WindowGroup("Auto2FA Logs", id: "logs") {
            LogViewerView()
                .environmentObject(appState)
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
        let window = NSApp.windows.first { $0.isVisible && $0.title == "Auto2FA" }
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
            appState.notchPresenter.show(
                systemImage: "network",
                title: "Network changed",
                description: "Probing tunnels…",
                tint: .yellow
            )
            Task {
                try? await appState.client.wakeRecover()
                await appState.reloadAll()
            }
        }
        mon.start()
        networkMonitor = mon
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
                    NSLog("[Auto2FA] wake observed; autoRecoverOnWake=off, skipping")
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
                        NSLog("[Auto2FA] wake_recover dispatched")
                    } catch {
                        NSLog("[Auto2FA] wake_recover failed: \(error.localizedDescription)")
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
