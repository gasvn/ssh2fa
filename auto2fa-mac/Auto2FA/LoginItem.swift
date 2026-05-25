import Foundation
import ServiceManagement

/// Wraps SMAppService.mainApp (macOS 13+) so a SwiftUI Toggle can register /
/// unregister this app for launch-at-login.
///
/// IMPORTANT path caveat: SMAppService remembers the bundle URL at register
/// time. If you move the .app afterwards, login launch silently breaks until
/// you re-toggle. For dev builds in build/Build/Products/Debug/Auto2FA.app
/// that's fine for testing but fragile — for everyday use, ship the .app to
/// /Applications first.
enum LoginItem {
    /// Whether macOS will start Auto2FA at the user's next login.
    static var isEnabled: Bool {
        if #available(macOS 13.0, *) {
            return SMAppService.mainApp.status == .enabled
        }
        return false
    }

    /// True if we know we can't satisfy this request on this OS version.
    static var isSupported: Bool {
        if #available(macOS 13.0, *) { return true }
        return false
    }

    /// Returns a user-readable status sentence (Enabled / Disabled / "Bundle
    /// path changed — re-enable to fix" / "Requires macOS 13+").
    static var statusDescription: String {
        if #available(macOS 13.0, *) {
            switch SMAppService.mainApp.status {
            case .notRegistered: return "Not registered"
            case .enabled:       return "Enabled — will start at next login"
            case .requiresApproval: return "Requires approval in System Settings → Login Items"
            case .notFound:      return "Bundle not found — re-toggle to refresh"
            @unknown default:    return "Unknown"
            }
        } else {
            return "Requires macOS 13+"
        }
    }

    /// Try to set the state. Returns nil on success, error message on
    /// failure (typically: bundle is unsigned in an unusable way, or the
    /// user hasn't moved the app to /Applications).
    @discardableResult
    static func setEnabled(_ on: Bool) -> String? {
        guard #available(macOS 13.0, *) else { return "Requires macOS 13+" }
        do {
            if on {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            return nil
        } catch {
            return error.localizedDescription
        }
    }
}
