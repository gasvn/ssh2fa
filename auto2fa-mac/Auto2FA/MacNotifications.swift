import Foundation
import UserNotifications
import AppKit

/// Persistent macOS Notification Center toasts for events the user should
/// know about even when focused on another app — chiefly tunnel failures.
/// Notifications carry inline action buttons ("Restart", "Open Activity")
/// so the user can react without first switching to the app.
///
/// First post triggers a permission prompt. Denial is swallowed silently.
enum MacNotifications {
    /// Cached GRANT result (not just "we asked"): the old `requestedAuth` flag
    /// returned true forever even when the user DENIED, and was an
    /// unsynchronized static mutated from concurrent Tasks. @MainActor
    /// serializes it; caching only a definitive answer lets a transient error
    /// retry later.
    @MainActor private static var authResult: Bool?
    private static let categoryTunnelFail = "auto2fa.tunnelFail"
    private static let actionRestart = "auto2fa.restart"
    private static let actionShowActivity = "auto2fa.showActivity"

    /// Install the notification category. Called once at app launch.
    static func registerCategories() {
        let restart = UNNotificationAction(
            identifier: actionRestart, title: "Restart",
            options: [.foreground]
        )
        let show = UNNotificationAction(
            identifier: actionShowActivity, title: "Show Activity",
            options: [.foreground]
        )
        let cat = UNNotificationCategory(
            identifier: categoryTunnelFail,
            actions: [restart, show],
            intentIdentifiers: [],
            options: [.customDismissAction]
        )
        UNUserNotificationCenter.current().setNotificationCategories([cat])
    }

    @MainActor
    static func ensureAuthorized() async -> Bool {
        if let cached = authResult { return cached }
        do {
            let granted = try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound])
            authResult = granted
            return granted
        } catch {
            return false
        }
    }

    /// Post a tunnel-fail notification with Restart + Show Activity buttons.
    /// `tunnelName` is stuffed into userInfo so the delegate can route the
    /// action click back to the right tunnel.
    @MainActor
    static func postTunnelFailed(name: String, body: String) {
        Task { _ = await ensureAuthorized() }
        let content = UNMutableNotificationContent()
        content.title = "Tunnel \(name) failed"
        content.body = body
        content.sound = .default
        content.categoryIdentifier = categoryTunnelFail
        content.userInfo = ["tunnel": name]
        let req = UNNotificationRequest(identifier: "tunnel.\(name)",
                                        content: content, trigger: nil)
        UNUserNotificationCenter.current().add(req) { err in
            if let err {
                NSLog("[Auto2FA] notification post failed: \(err.localizedDescription)")
            }
        }
    }

    /// Plain notification for generic events (no actions).
    @MainActor
    static func post(title: String, body: String, identifier: String? = nil) {
        Task { _ = await ensureAuthorized() }
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        let id = identifier ?? title
        let req = UNNotificationRequest(identifier: id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(req)
    }
}

/// Routes notification action clicks (Restart / Show Activity buttons)
/// back into AppState. Must be installed as UNUserNotificationCenter
/// delegate before any notifications are scheduled.
@MainActor
final class NotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
    static let shared = NotificationDelegate()
    weak var appState: AppState?

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let id = response.actionIdentifier
        let userInfo = response.notification.request.content.userInfo
        let name = userInfo["tunnel"] as? String
        Task { @MainActor in
            guard let name, let state = self.appState else {
                completionHandler()
                return
            }
            if id == "auto2fa.restart" {
                // We must NOT rely on a stale snapshot of displayState
                // here — the old impl read tunnel.displayState captured
                // at notification-fire time, which meant Restart was
                // either a no-op or a stop, never a real restart.
                // Drive directly: stop if currently alive (refetched),
                // then unconditionally start.
                guard let fresh = state.tunnels.first(where: { $0.name == name }) else {
                    completionHandler(); return
                }
                if fresh.displayState == .alive {
                    await state.toggleTunnel(fresh)
                    // Refetch so we know it's stopped now
                    try? await Task.sleep(nanoseconds: 100_000_000)
                }
                // Use the up-to-date snapshot for the start
                if let nowFresh = state.tunnels.first(where: { $0.name == name }),
                   nowFresh.displayState != .alive {
                    await state.toggleTunnel(nowFresh)
                }
            } else if id == "auto2fa.showActivity" {
                // Bring main window forward + signal TunnelsView to open
                // the details popover for this tunnel.
                NSApp.activate(ignoringOtherApps: true)
                if let w = NSApp.windows.first(where: { $0.title == "Auto2FA" }) {
                    w.makeKeyAndOrderFront(nil)
                }
                NotificationCenter.default.post(
                    name: .a2fShowTunnelDetails, object: nil,
                    userInfo: ["name": name]
                )
            }
            completionHandler()
        }
    }

    // Show notifications even when the app is foregrounded.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }
}
