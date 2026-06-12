import SwiftUI
import AppKit

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

// MARK: - IconActionButton style

/// Shared button style for the inline icon quick-actions in dense rows.
/// Gives each icon a subtle rounded hover/press background so it reads as
/// interactive (HIG affordance), without enlarging the row. Disabled buttons
/// dim and show no hover background. Presentation only — no behaviour change.
struct IconActionButton: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let active = configuration.isPressed || hovering
        configuration.label
            .frame(width: 22, height: 20)
            .foregroundStyle(isEnabled ? AnyShapeStyle(.primary) : AnyShapeStyle(.tertiary))
            .background(
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .fill(Color.primary.opacity((active && isEnabled) ? 0.10 : 0.0))
            )
            .contentShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
            .opacity(configuration.isPressed ? 0.7 : 1.0)
            .onHover { if isEnabled { hovering = $0 } else { hovering = false } }
            .animation(.easeOut(duration: 0.12), value: active)
    }
}

/// Shared button style for the hover-revealed icon+TEXT action labels in dense
/// rows (the 2026 dense-row pattern). Same subtle rounded hover/press
/// background as `IconActionButton`, but sized for a compact `Label` (icon +
/// one short word) instead of an icon-only square. Disabled buttons dim and
/// show no hover background. Presentation only — no behaviour change.
struct IconTextActionButton: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let active = configuration.isPressed || hovering
        configuration.label
            .labelStyle(.titleAndIcon)
            .font(.caption)
            .lineLimit(1)
            .fixedSize()
            .padding(.horizontal, 7)
            .frame(height: 20)
            .foregroundStyle(isEnabled ? AnyShapeStyle(.primary) : AnyShapeStyle(.tertiary))
            .background(
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .fill(Color.primary.opacity((active && isEnabled) ? 0.10 : 0.0))
            )
            .contentShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
            .opacity(configuration.isPressed ? 0.7 : 1.0)
            .onHover { if isEnabled { hovering = $0 } else { hovering = false } }
            .animation(.easeOut(duration: 0.12), value: active)
    }
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

    // MARK: Liquid Glass surfaces (macOS 26 — no fallback; app is 26-only)

    /// Primary elevated FLOATING surface — cards / snackbars / banners that hover
    /// above content. Real Liquid Glass.
    func glassCard(cornerRadius: CGFloat = Radius.card) -> some View {
        self.glassEffect(.regular, in: .rect(cornerRadius: cornerRadius, style: .continuous))
    }

    /// Content section surface — TRANSPARENT so list rows float directly on the
    /// window's frosted glass pane (the wallpaper shows through). No opaque fill;
    /// just a continuous rounded clip + a faint glass-edge hairline. The frosted
    /// window material behind it is what keeps row text legible.
    func groupedContent(cornerRadius: CGFloat = Radius.card) -> some View {
        self
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .strokeBorder(.white.opacity(0.10), lineWidth: 1)
            )
    }

    /// Lighter glass for floating chrome — bars / palettes.
    func glassChrome(cornerRadius: CGFloat = Radius.control) -> some View {
        self.glassEffect(.regular, in: .rect(cornerRadius: cornerRadius, style: .continuous))
    }

    /// One interactive, semantically-tinted glass surface for hero controls.
    func interactiveGlass(tint: Color? = nil, cornerRadius: CGFloat = Radius.control) -> some View {
        let glass: Glass = tint.map { .regular.tint($0).interactive() } ?? .regular.interactive()
        return self.glassEffect(glass, in: .rect(cornerRadius: cornerRadius, style: .continuous))
    }

    /// Frosted glass window pane — blurs the DESKTOP behind the window (needs
    /// `transparentWindow()` so the NSWindow is non-opaque, else it samples a
    /// flat gray backing). This frosted pane is the single glass surface the
    /// whole UI floats on.
    func windowGlassBackground() -> some View {
        self.background(
            VisualEffectBackground(material: .underWindowBackground,
                                   blending: .behindWindow)
                .ignoresSafeArea()
        )
    }

    /// Make the hosting NSWindow non-opaque + clear so the `.behindWindow`
    /// frosted material actually shows the desktop/wallpaper (the core of the
    /// floating Liquid Glass look). Without this the window stays opaque and the
    /// material renders flat gray.
    func transparentWindow() -> some View {
        self.background(WindowConfigurator())
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

// MARK: - Translucent material backing (AppKit bridge)

/// Thin wrapper over `NSVisualEffectView` so a SwiftUI view can sit on a
/// real desktop-sampling translucent material — the basis of the floating
/// window look. SwiftUI has no first-class equivalent on macOS.
struct VisualEffectBackground: NSViewRepresentable {
    let material: NSVisualEffectView.Material
    let blending: NSVisualEffectView.BlendingMode

    func makeNSView(context: Context) -> NSVisualEffectView {
        let v = NSVisualEffectView()
        v.material = material
        v.blendingMode = blending
        v.state = .active
        return v
    }

    func updateNSView(_ v: NSVisualEffectView, context: Context) {
        v.material = material
        v.blendingMode = blending
    }
}

/// Reaches the hosting NSWindow and makes it non-opaque + clear-backed so a
/// `.behindWindow` visual-effect material samples the actual desktop (wallpaper
/// shows through, frosted). Without this the window's opaque backing turns the
/// "translucent" material into flat gray.
struct WindowConfigurator: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let v = NSView()
        DispatchQueue.main.async { [weak v] in configure(v?.window) }
        return v
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        DispatchQueue.main.async { [weak nsView] in configure(nsView?.window) }
    }

    private func configure(_ window: NSWindow?) {
        guard let window else { return }
        window.isOpaque = false
        window.backgroundColor = .clear
    }
}
