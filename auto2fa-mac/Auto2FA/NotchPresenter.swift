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
    private var inFlight: Task<Void, Never>?
    private var current: DynamicNotchInfo?

    func show(systemImage: String, title: String, description: String, tint: Color = .primary) {
        // Honor the "Show Dynamic Notch toasts" setting at the single
        // chokepoint — ~20 call sites (errors, sleep/wake, copy, import…)
        // bypassed the per-tunnel check and toasted even when disabled.
        let enabled = UserDefaults.standard.object(forKey: SettingsKey.notchEnabled) as? Bool ?? true
        guard enabled else { return }
        // If a previous notch is still on screen, hide it so we don't pile up
        // overlapping animations.
        inFlight?.cancel()
        if let existing = current {
            Task { @MainActor in await existing.hide() }
        }

        let info = DynamicNotchInfo(
            icon: .init(systemName: systemImage, color: tint),
            title: LocalizedStringKey(stringLiteral: title),
            description: LocalizedStringKey(stringLiteral: description)
        )
        current = info

        inFlight = Task { @MainActor in
            await info.expand()
            try? await Task.sleep(nanoseconds: 3_500_000_000)
            if !Task.isCancelled {
                await info.hide()
            }
        }
    }
}
