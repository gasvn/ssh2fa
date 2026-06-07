import SwiftUI

/// A small status indicator dot. Shows:
/// - A pulsing filled circle for `working` states (connecting / starting).
/// - An exclamation-mark triangle for `attention` states (failed / stale / portBusy).
/// - A plain filled circle for `ok` (connected / alive) and `idle` states.
///
/// This is the single shared status indicator for the whole app (it replaced
/// the per-view `PulsingDot` implementations).
struct StatusDot: View {
    // Internal severity bucket — collapses the two model enums into four
    // universal categories so colour + animation logic stays in one place.
    private enum Kind {
        case ok, working, attention, idle
    }

    private let kind:  Kind
    private let color: Color

    // MARK: - Convenience initialisers

    init(host: SSHHost.DisplayState) {
        switch host {
        case .connected:  kind = .ok;        color = StatusColor.host(host)
        case .connecting: kind = .working;   color = StatusColor.host(host)
        case .failed:     kind = .attention; color = StatusColor.host(host)
        case .stopped:    kind = .idle;      color = StatusColor.host(host)
        case .unknown:    kind = .idle;      color = StatusColor.host(host)
        }
    }

    init(tunnel: Tunnel.DisplayState) {
        switch tunnel {
        case .alive:    kind = .ok;        color = StatusColor.tunnel(tunnel)
        case .starting: kind = .working;   color = StatusColor.tunnel(tunnel)
        case .stale:    kind = .attention; color = StatusColor.tunnel(tunnel)
        case .portBusy: kind = .attention; color = StatusColor.tunnel(tunnel)
        case .failed:   kind = .attention; color = StatusColor.tunnel(tunnel)
        case .idle:     kind = .idle;      color = StatusColor.tunnel(tunnel)
        case .unknown:  kind = .idle;      color = StatusColor.tunnel(tunnel)
        }
    }

    // MARK: - Body

    @State private var phase: Bool = false

    var body: some View {
        Group {
            switch kind {
            case .attention:
                Image(systemName: "exclamationmark.triangle.fill")
                    .resizable()
                    .scaledToFit()
                    .foregroundStyle(.red)

            case .working:
                Circle()
                    .fill(color)
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
            }
        }
        .frame(width: RowMetric.iconSize, height: RowMetric.iconSize)
    }
}
