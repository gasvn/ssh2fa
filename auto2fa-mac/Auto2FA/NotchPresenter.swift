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

    func show(systemImage: String, title: String, description: String, tint: Color) {
        inFlight?.cancel()
        inFlight = Task { @MainActor in
            let info = DynamicNotchInfo(
                icon: Image(systemName: systemImage)
                    .foregroundStyle(tint) as AnyView? ?? AnyView(EmptyView()),
                title: title,
                description: description
            )
            await info.expand()
            try? await Task.sleep(nanoseconds: 3_500_000_000)
            await info.compact()
        }
    }
}
