import SwiftUI

/// Subtle yellow background pulse when `value` changes. Used to make
/// table cells "alive" — when a tunnel's status flips from idle to
/// connecting, the row cell briefly highlights so the user's eye is
/// drawn to it without having to stare. ~500ms fade, then transparent.
///
/// Cheap: only one Task per change, no continuous animation.
struct ChangeHighlightModifier<Value: Equatable>: ViewModifier {
    let value: Value
    @State private var highlight: Double = 0  // alpha 0..1

    func body(content: Content) -> some View {
        content
            .background(
                Color.yellow.opacity(0.25 * highlight)
                    .animation(.easeOut(duration: 0.5), value: highlight)
            )
            .onChange(of: value) { _, _ in
                highlight = 1
                Task {
                    try? await Task.sleep(nanoseconds: 600_000_000)
                    await MainActor.run { highlight = 0 }
                }
            }
    }
}

extension View {
    /// Flash a yellow background briefly whenever `value` changes.
    func changeHighlight<V: Equatable>(_ value: V) -> some View {
        self.modifier(ChangeHighlightModifier(value: value))
    }
}
