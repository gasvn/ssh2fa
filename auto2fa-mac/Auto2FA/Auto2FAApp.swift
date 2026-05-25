import SwiftUI
import AppKit

@main
struct Auto2FAApp: App {
    @StateObject private var appState = AppState()
    @StateObject private var menuBar = MenuBarController()

    var body: some Scene {
        WindowGroup("Auto2FA") {
            ContentView()
                .environmentObject(appState)
                .onAppear { installMenuBarOnce() }
        }
        .defaultSize(width: 820, height: 540)
        .windowToolbarStyle(.unifiedCompact)
        .commands {
            // Replace the standard File → New so ⌘N opens our New Tunnel sheet.
            // The toolbar button in ContentView calls the same action without
            // a keyboard shortcut to avoid clashing with this one.
            CommandGroup(replacing: .newItem) {
                Button("New Tunnel…") {
                    appState.presentNewTunnel()
                }
                .keyboardShortcut("n", modifiers: [.command])
            }
            // Help → Auto2FA on GitHub
            CommandGroup(replacing: .help) {
                Link("Auto2FA on GitHub",
                     destination: URL(string: "https://github.com/gasvn/auto2fa")!)
            }
        }
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
}
