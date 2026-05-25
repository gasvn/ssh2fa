import SwiftUI
import AppKit

@main
struct Auto2FAApp: App {
    @StateObject private var appState = AppState()
    @StateObject private var menuBar = MenuBarController()
    // Kept as an instance var so it isn't deallocated and unsubscribes.
    @State private var sleepWakeMonitor: SleepWakeMonitor?

    var body: some Scene {
        WindowGroup("Auto2FA") {
            ContentView()
                .environmentObject(appState)
                .onAppear {
                    SingleInstance.enforceOrExit()
                    installMenuBarOnce()
                    installSleepWakeMonitor()
                }
                .task {
                    // Spawn daemon first (or detect existing one), THEN run
                    // AppState.bootstrap. Doing this serially in a single
                    // .task avoids the race where ContentView's own .task
                    // would fire bootstrap before we've started the daemon.
                    let result = await DaemonProcess.shared.ensureRunning()
                    switch result {
                    case .alreadyRunning:
                        NSLog("[Auto2FA] daemon was already running")
                    case .spawned(let pid):
                        NSLog("[Auto2FA] spawned daemon, PID=\(pid)")
                    case .failed(let reason):
                        appState.connectionError = reason
                        return  // don't try to bootstrap against nothing
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
