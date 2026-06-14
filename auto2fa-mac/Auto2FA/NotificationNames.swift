import Foundation

/// Cross-component bus. NotificationCenter is overkill for app-internal
/// signals but it's the lightest way to fire-and-forget into a SwiftUI
/// view from anywhere (Auto2FAApp.commands, MenuBarController, etc.)
/// without threading an EnvironmentObject through every type.
extension Notification.Name {
    /// Open the ⌘⇧P command palette. Posted by File menu and the menu bar.
    static let a2fShowPalette = Notification.Name("auto2fa.showPalette")
    /// Open the in-app daemon log viewer.
    static let a2fShowLogs = Notification.Name("auto2fa.showLogs")
    /// Open the per-tunnel details popover. userInfo["name"] = tunnel name.
    static let a2fShowTunnelDetails = Notification.Name("auto2fa.showTunnelDetails")
    /// Open the SwiftUI Settings scene. The legacy `showSettingsWindow:`
    /// selector is a no-op on macOS 26, so AppKit (MenuBarController) posts this
    /// for a SwiftUI view to handle via `@Environment(\.openSettings)`.
    static let a2fShowSettings = Notification.Name("auto2fa.showSettings")
}
