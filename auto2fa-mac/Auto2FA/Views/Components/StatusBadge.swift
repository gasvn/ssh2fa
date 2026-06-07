import SwiftUI

/// A compact HStack: `StatusDot` + a caption text label in the status colour.
/// Mirrors `StatusDot`'s two convenience initialisers (host / tunnel).
struct StatusBadge: View {
    private let dot:   StatusDot
    private let label: String
    private let color: Color

    // MARK: - Convenience initialisers

    init(host: SSHHost.DisplayState, text: String) {
        self.dot   = StatusDot(host: host)
        self.label = text
        self.color = StatusColor.host(host)
    }

    init(tunnel: Tunnel.DisplayState, text: String) {
        self.dot   = StatusDot(tunnel: tunnel)
        self.label = text
        self.color = StatusColor.tunnel(tunnel)
    }

    // MARK: - Body

    var body: some View {
        HStack(spacing: Spacing.xs) {
            dot
            Text(label)
                .font(.caption)
                .foregroundStyle(color)
        }
    }
}
