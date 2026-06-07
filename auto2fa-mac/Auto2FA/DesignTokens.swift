import SwiftUI

// MARK: - Spacing

enum Spacing {
    static let xs: CGFloat = 4
    static let s:  CGFloat = 8
    static let m:  CGFloat = 12
    static let l:  CGFloat = 16
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
}
