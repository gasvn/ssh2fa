import Foundation
import SwiftUI
import DynamicNotchKit

/// Long-lived notch indicator that summarises overall tunnel state.
///
/// Behavior:
///   - When any tunnel is `alive` or `starting`, or any host is failed,
///     a compact `DynamicNotchInfo` sits over the notch.
///   - Hover/click on the notch expands it (DynamicNotchKit handles this).
///   - When everything is idle, we hide the notch entirely so we're not
///     visually intrusive when the user isn't actively using auto2fa.
///
/// Opt-in: gated behind the `notchPersistent` setting because some users
/// will find a persistent overlay annoying.
///
/// We keep this separate from NotchPresenter (toast-style) because the
/// lifecycles are different — toasts expand→hide in seconds, the
/// persistent one stays put and only re-renders content.
@MainActor
final class PersistentNotchController {
    private var current: DynamicNotchInfo?
    private var lastSignature: String = ""
    private(set) var enabled = false

    /// Pull the latest state from AppState and (re)render the persistent
    /// notch. Cheap to call — re-renders only when the content signature
    /// actually changes.
    func update(from appState: AppState) {
        // Honor the user's master kill-switch.
        let on = UserDefaults.standard.object(forKey: SettingsKey.notchEnabled) as? Bool ?? true
        let persistent = UserDefaults.standard.bool(forKey: SettingsKey.notchPersistent)
        enabled = on && persistent
        guard enabled else {
            Task { @MainActor in await self.current?.hide() }
            current = nil
            lastSignature = ""
            return
        }

        let aliveCount = appState.tunnels.filter { $0.displayState == .alive }.count
        let startingCount = appState.tunnels.filter { $0.displayState == .starting }.count
        let failedCount = appState.tunnels.filter {
            $0.displayState == .failed || $0.displayState == .portBusy
        }.count
        let totalActive = aliveCount + startingCount + failedCount

        // Nothing interesting → hide.
        if totalActive == 0 {
            Task { @MainActor in await self.current?.hide() }
            current = nil
            lastSignature = ""
            return
        }

        // Decide tint + title.
        let title: String
        let tint: Color
        let icon: String
        if failedCount > 0 {
            tint = .red
            icon = "exclamationmark.triangle.fill"
            title = "\(failedCount) failed"
        } else if startingCount > 0 {
            tint = .yellow
            icon = "arrow.triangle.2.circlepath"
            title = "\(startingCount) connecting"
        } else {
            tint = .green
            icon = "bolt.fill"
            title = "\(aliveCount) alive"
        }
        let signature = "\(icon)|\(title)|\(failedCount)|\(startingCount)|\(aliveCount)"
        if signature == lastSignature { return }
        lastSignature = signature

        // Build a fresh DynamicNotchInfo. We dispose+recreate because the
        // API doesn't expose live content mutation — each compact view is
        // built at construction time.
        let info = DynamicNotchInfo(
            icon: .init(systemName: icon, color: tint),
            title: LocalizedStringKey(stringLiteral: title),
            description: LocalizedStringKey(stringLiteral: "Auto2FA")
        )
        let prev = current
        current = info
        Task { @MainActor in
            // Hide old before showing new so we don't stack two overlays.
            if let prev { await prev.hide() }
            await info.compact()
        }
    }

    /// Explicit teardown for app shutdown.
    func hide() {
        Task { @MainActor in await current?.hide() }
        current = nil
        lastSignature = ""
    }
}
