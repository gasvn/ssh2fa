import SwiftUI

@main
struct Auto2FAApp: App {
    @StateObject private var appState = AppState()
    @StateObject private var menuBar = MenuBarController()

    var body: some Scene {
        WindowGroup("Auto2FA") {
            ContentView()
                .environmentObject(appState)
                .onAppear {
                    if let window = NSApplication.shared.windows.first {
                        menuBar.install(appState: appState, window: window)
                    }
                }
        }
        .defaultSize(width: 800, height: 500)
        .windowToolbarStyle(.unifiedCompact)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Tunnel…") {
                    // wire up in next session — opens NewTunnelSheet
                }
                .keyboardShortcut("n", modifiers: [.command])
            }
        }
    }
}
