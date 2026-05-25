import Foundation
import SwiftUI
// DynamicNotchKit must be added via Swift Package Manager:
//   https://github.com/MrKai77/DynamicNotchKit
// See auto2fa-mac/README.md.
import DynamicNotchKit

/// Wraps DynamicNotchKit so the rest of the app calls one async method.
///
/// On notched MacBooks the toast animates out of the notch; on Macs without
/// a notch (Air, Intel) DynamicNotchKit automatically falls back to a
/// floating panel.
@MainActor
final class NotchPresenter: ObservableObject {
    private var inFlight: Task<Void, Never>?

    /// `tint` is kept on the API for callers, but the most version-portable
    /// way to use DynamicNotchInfo is to pass just a system-image name. We
    /// pick semantically-different SF Symbols per call site to convey
    /// success/warn/error rather than applying foregroundStyle to a generic
    /// icon — different versions of DynamicNotchInfo wrap the icon
    /// differently (Image vs. View vs. IconStyle), and passing a styled
    /// `some View` breaks the type. Leaving `tint` unused is intentional.
    func show(systemImage: String, title: String, description: String, tint: Color = .primary) {
        _ = tint  // see note above; reserved for a future bespoke DynamicNotch view
        inFlight?.cancel()
        inFlight = Task { @MainActor in
            let info = DynamicNotchInfo(
                icon: Image(systemName: systemImage),
                title: title,
                description: description
            )
            await info.expand()
            try? await Task.sleep(nanoseconds: 3_500_000_000)
            await info.compact()
        }
    }
}
