import SwiftUI

// MARK: - Spacing

enum Spacing {
    static let xs: CGFloat = 4
    static let s:  CGFloat = 8
    static let m:  CGFloat = 12
    static let l:  CGFloat = 16
    static let xl: CGFloat = 24
}

// MARK: - Radius

/// Continuous corner radii for the 2026 "Liquid Glass" look.
enum Radius {
    static let card:    CGFloat = 20
    static let pill:    CGFloat = 999
    static let control: CGFloat = 10
}

// MARK: - Brand

/// Single place to swap the accent later. Restrained: tracks the system accent.
enum Brand {
    static let accent = Color.accentColor
}

// MARK: - Rounded font helpers

extension Font {
    /// Dashboard / section title — friendly rounded, semibold.
    static var dashTitle: Font {
        .system(.headline, design: .rounded).weight(.semibold)
    }

    /// Count badges (e.g. host / tunnel counts) — rounded, semibold.
    static var countBadge: Font {
        .system(.caption, design: .rounded).weight(.semibold)
    }

    /// Row titles (host names, tunnel names) — the most prominent element in a
    /// row. Rounded, semibold, title3-sized so it clearly outranks the
    /// secondary identifiers and tertiary metadata beneath it.
    static var rowTitle: Font {
        .system(.title3, design: .rounded).weight(.semibold)
    }

    /// Secondary technical identifiers (hostname, :port→:port, node). Monospaced
    /// callout — legible, clearly a step below the row title.
    static var rowIdentifier: Font {
        .system(.callout, design: .monospaced)
    }

    /// Tertiary metadata (aliveSince, via, fails). Footnote weight regular.
    static var rowMeta: Font {
        .footnote
    }
}

// MARK: - StatusColor

enum StatusColor {
    static func host(_ s: SSHHost.DisplayState) -> Color {
        switch s {
        case .connected:  return .green
        case .connecting: return .yellow
        case .failed:     return .red
        case .stopped:    return .secondary
        case .unknown:    return .secondary
        }
    }

    static func tunnel(_ s: Tunnel.DisplayState) -> Color {
        switch s {
        case .alive:     return .green
        case .starting:  return .yellow
        case .stale:     return .red
        case .portBusy:  return .red
        case .failed:    return .red
        case .idle:      return .secondary
        case .unknown:   return .secondary
        }
    }

    // Aliases for tinted pills — same colors, friendlier call sites.
    static func tint(forHost s: SSHHost.DisplayState) -> Color { host(s) }
    static func tint(forTunnel s: Tunnel.DisplayState) -> Color { tunnel(s) }
}

// MARK: - RowMetric

enum RowMetric {
    static let vPad:      CGFloat = 6
    static let minHeight: CGFloat = 28
    static let iconSize:  CGFloat = 13
    static let mono: Font = .system(.body, design: .monospaced)
}

// MARK: - View modifiers

extension View {
    /// Uppercase caption, secondary color, small letter spacing — used for
    /// section headers throughout the dashboard.
    func sectionHeaderStyle() -> some View {
        self
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
            .kerning(0.5)
            .textCase(.uppercase)
            .padding(.horizontal, Spacing.m)
            .padding(.vertical, Spacing.xs)
    }

    /// Consistent vertical padding for list rows.
    func dashboardRow() -> some View {
        self.padding(.vertical, RowMetric.vPad)
    }

    // MARK: Liquid Glass surfaces (macOS 26) with material fallback (14.0+)

    /// Primary elevated surface — cards / panels. Uses Liquid Glass on
    /// macOS 26, falls back to a bordered + shadowed material on older systems.
    @ViewBuilder
    func glassCard(cornerRadius: CGFloat = Radius.card) -> some View {
        if #available(macOS 26.0, *) {
            self.glassEffect(.regular, in: .rect(cornerRadius: cornerRadius))
        } else {
            self
                .background(
                    .regularMaterial,
                    in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                        .strokeBorder(.white.opacity(0.08))
                )
                .shadow(color: .black.opacity(0.12), radius: 10, y: 4)
        }
    }

    /// Quiet, OPAQUE grouped content surface for list sections — the BASE
    /// layer. Continuous rounded corners + a hairline border, NO blur / NO
    /// glass. This is what content (hosts/tunnels lists) sits in; glass is
    /// reserved for floating chrome above content.
    func groupedContent(cornerRadius: CGFloat = Radius.card) -> some View {
        self
            .background(
                Color(nsColor: .controlBackgroundColor),
                in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
            )
            .overlay(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .strokeBorder(Color(nsColor: .separatorColor).opacity(0.5), lineWidth: 1)
            )
    }

    /// Lighter glass for chrome — toolbars / bars. Thinner material fallback.
    @ViewBuilder
    func glassChrome() -> some View {
        if #available(macOS 26.0, *) {
            self.glassEffect(.regular, in: .rect(cornerRadius: Radius.control))
        } else {
            self.background(.ultraThinMaterial)
        }
    }

    /// Subtle hover elevation — gentle scale + soft shadow, animated.
    func hoverLift(_ hovering: Bool) -> some View {
        self
            .scaleEffect(hovering ? 1.005 : 1.0)
            .shadow(
                color: .black.opacity(hovering ? 0.18 : 0.0),
                radius: hovering ? 8 : 0,
                y: hovering ? 3 : 0
            )
            .animation(.easeOut(duration: 0.16), value: hovering)
    }
}
