import Foundation
import UserNotifications
import AppKit

/// Persistent macOS Notification Center toasts for events the user should
/// know about even when they're focused on another app — chiefly tunnel
/// failures. Dynamic Notch covers the "ambient feedback while I'm watching"
/// case; this covers "tell me when I come back".
///
/// First post triggers a permission prompt. We swallow denial silently —
/// the Dynamic Notch path still works, and we don't want to spam the user
/// every time they reject the prompt.
enum MacNotifications {
    private static var requestedAuth = false

    static func ensureAuthorized() async -> Bool {
        if requestedAuth { return true }
        requestedAuth = true
        do {
            return try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound])
        } catch {
            return false
        }
    }

    /// Schedule a notification. Coalesces by title so repeat failures for
    /// the same tunnel don't pile up.
    @MainActor
    static func post(title: String, body: String, identifier: String? = nil) {
        Task { _ = await ensureAuthorized() }
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        let id = identifier ?? title  // coalesce per title
        let req = UNNotificationRequest(identifier: id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(req) { err in
            if let err {
                NSLog("[Auto2FA] notification post failed: \(err.localizedDescription)")
            }
        }
    }
}
