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
    /// Serialise show/hide operations into a single chained Task so
    /// rapid update() calls can't race two `compact()`s on top of each
    /// other (briefly stacking notches visually). Each new operation
    /// awaits the previous one before running.
    private var pendingOp: Task<Void, Never>?

    /// Pull the latest state from AppState and (re)render the persistent
    /// notch. Cheap to call — re-renders only when the content signature
    /// actually changes.
    func update(from appState: AppState) {
        // Honor the user's master kill-switch.
        let on = UserDefaults.standard.object(forKey: SettingsKey.notchEnabled) as? Bool ?? true
        let persistent = UserDefaults.standard.bool(forKey: SettingsKey.notchPersistent)
        enabled = on && persistent
        guard enabled else {
            let prev = current
            current = nil
            lastSignature = ""
            enqueue { await prev?.hide() }
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
            let prev = current
            current = nil
            lastSignature = ""
            enqueue { await prev?.hide() }
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
            description: LocalizedStringKey(stringLiteral: "SSH2FA")
        )
        let prev = current
        current = info
        enqueue {
            if let prev { await prev.hide() }
            await info.compact()
        }
    }

    /// Explicit teardown for app shutdown.
    func hide() {
        let prev = current
        current = nil
        lastSignature = ""
        enqueue { await prev?.hide() }
    }

    /// Chain a new async operation after the previous one so they don't
    /// race. Each call returns immediately; the work happens in order
    /// on the actor.
    private func enqueue(_ op: @escaping @MainActor () async -> Void) {
        let prev = pendingOp
        pendingOp = Task { @MainActor in
            _ = await prev?.value  // wait for the previous op to finish
            await op()
        }
    }
}
