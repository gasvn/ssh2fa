import Foundation
import SwiftUI
// DynamicNotchKit must be added via Swift Package Manager:
//   https://github.com/MrKai77/DynamicNotchKit  (pinned to >= 1.0.0)
// See auto2fa-mac/README.md.
import DynamicNotchKit

/// Wraps DynamicNotchKit so the rest of the app calls one async method.
///
/// On notched MacBooks the toast animates from the notch; on Macs without
/// a notch (Air, Intel, external display) DynamicNotchKit's `.auto` style
/// automatically falls back to a floating panel.
///
/// API contract verified against tag 1.0.0 (and current main) of DynamicNotchKit:
///   - DynamicNotchInfo.init(icon: Label?, title: LocalizedStringKey, ...)
///   - Label has .init(systemName:color:)
///   - .expand(), .hide() are the show/dismiss pair (compact() shrinks but
///     stays in the notch as a small island; for a transient toast we want
///     expand → hide)
@MainActor
final class NotchPresenter: ObservableObject {
    private var current: DynamicNotchInfo?
    /// Auto-hide timer for the on-screen toast. Cancelled (and replaced) on
    /// every new show() so a fresh toast resets the 3.5 s window.
    private var hideTimer: Task<Void, Never>?
    /// Serializes the actual expand()/hide() ANIMATION calls. The old code
    /// hid the previous notch in a DETACHED Task that ran concurrently with
    /// the new notch's expand() — two overlapping DynamicNotchKit animations,
    /// which under a burst of show()s produced a visible flicker/glitch. Each
    /// transition now awaits the previous one. Transitions are quick (the
    /// 3.5 s wait lives in hideTimer, NOT here), so a newer toast still
    /// replaces an older one immediately — no queueing.
    private var transition: Task<Void, Never>?

    func show(systemImage: String, title: String, description: String, tint: Color = .primary) {
        // Honor the "Show Dynamic Notch toasts" setting at the single
        // chokepoint — ~20 call sites (errors, sleep/wake, copy, import…)
        // bypassed the per-tunnel check and toasted even when disabled.
        let enabled = UserDefaults.standard.object(forKey: SettingsKey.notchEnabled) as? Bool ?? true
        guard enabled else { return }

        // Do Not Disturb: show toasts as the COMPACT pill that hugs the notch
        // (icon + brief text, left/right) instead of EXPANDing a panel down.
        // Read once here and capture by value into the transition closure.
        let dnd = UserDefaults.standard.bool(forKey: SettingsKey.notchDoNotDisturb)

        hideTimer?.cancel()

        let info = DynamicNotchInfo(
            icon: .init(systemName: systemImage, color: tint),
            title: LocalizedStringKey(stringLiteral: title),
            description: LocalizedStringKey(stringLiteral: description)
        )
        let prev = current
        current = info

        // Serialize hide(prev) → show(new) so the two animations never overlap.
        let prevTransition = transition
        transition = Task { @MainActor in
            await prevTransition?.value
            if let prev { await prev.hide() }
            if dnd {
                await info.compact()   // DND: minimal pill around the notch
            } else {
                await info.expand()    // normal: full drop-down toast
            }
        }

        // Auto-hide after the dwell time — but only if THIS toast is still the
        // current one (a newer show() takes ownership of its own hide).
        hideTimer = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 3_500_000_000)
            guard !Task.isCancelled, self.current === info else { return }
            let pending = self.transition
            self.transition = Task { @MainActor in
                await pending?.value
                await info.hide()
            }
            if self.current === info { self.current = nil }
        }
    }
}
