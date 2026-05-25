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
}
