import SwiftUI

/// A tinted "pill" status indicator — the richer presentation of a status.
///
/// Renders a `Capsule` filled with the status colour at low opacity, a small
/// leading glyph (pulsing dot for working states, a warning triangle for
/// attention states, a plain dot otherwise) and a compact label, all in the
/// status colour. Mirrors `StatusDot`'s two convenience initialisers.
///
/// `StatusDot` remains the lighter indicator for very compact spots; this pill
/// is the main "feel" surface and reads well over glass in light and dark.
struct StatusBadge: View {
    // Local severity bucket — mirrors StatusDot's so the pill can choose its
    // leading glyph + animation without coupling to StatusDot internals.
    private enum Kind { case ok, working, attention, idle }

    private let kind:  Kind
    private let label: String
    private let color: Color

    // MARK: - Convenience initialisers

    init(host: SSHHost.DisplayState, text: String) {
        switch host {
        case .connected:  kind = .ok
        case .connecting: kind = .working
        case .failed:     kind = .attention
        case .stopped:    kind = .idle
        case .unknown:    kind = .idle
        }
        self.label = text
        self.color = StatusColor.tint(forHost: host)
    }

    init(tunnel: Tunnel.DisplayState, text: String) {
        switch tunnel {
        case .alive:    kind = .ok
        case .starting: kind = .working
        case .stale:    kind = .attention
        case .portBusy: kind = .attention
        case .failed:   kind = .attention
        case .idle:     kind = .idle
        case .unknown:  kind = .idle
        }
        self.label = text
        self.color = StatusColor.tint(forTunnel: tunnel)
    }

    // MARK: - Body

    @State private var phase: Bool = false

    private var symbolSize: CGFloat { 7 }

    @ViewBuilder
    private var leadingGlyph: some View {
        switch kind {
        case .attention:
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: symbolSize + 1))
                .foregroundStyle(color)

        case .working:
            Circle()
                .fill(color)
                .frame(width: symbolSize, height: symbolSize)
                .scaleEffect(phase ? 1.4 : 1.0)
                .opacity(phase ? 0.5 : 1.0)
                .animation(
                    .easeInOut(duration: 0.8).repeatForever(autoreverses: true),
                    value: phase
                )
                .onAppear { phase = true }

        case .ok, .idle:
            Circle()
                .fill(color)
                .frame(width: symbolSize, height: symbolSize)
        }
    }

    var body: some View {
        HStack(spacing: Spacing.xs) {
            leadingGlyph
            Text(label)
                .font(.system(.caption, design: .rounded).weight(.medium))
                .foregroundStyle(color)
        }
        .padding(.horizontal, Spacing.s)
        .padding(.vertical, 3)
        .background(color.opacity(0.15), in: Capsule())
    }
}
