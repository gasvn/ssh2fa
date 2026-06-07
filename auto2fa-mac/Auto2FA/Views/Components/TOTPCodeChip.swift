import SwiftUI

/// Inline, authenticator-style live 2FA (TOTP) chip for a host row.
///
/// Shows the current 6-digit code as two monospaced groups of three
/// (`832 194`) next to a small countdown ring that drains over the 30s
/// window and shifts colour as time runs out. Tap to copy the code.
///
/// REFRESH STRATEGY (minimise IPC + Keychain reads):
///   - Fetch the code ONCE per ~30s window via `appState.hostTOTP(host)`,
///     storing an absolute `expiry` Date and the period.
///   - A `TimelineView(.periodic(by: 0.5))` animates the ring LOCALLY off
///     that stored expiry — no IPC per tick. When the window elapses
///     (fraction hits 0) we re-fetch the next window's code.
///   - So: at most one daemon/Keychain round-trip per 30s window per host,
///     plus a cheap 0.5s local ring tick.
///
/// GRACEFUL FAILURE: if the fetch throws — no secret, daemon error, or a
/// pending/denied Keychain "Always Allow" prompt that makes the first read
/// slow and times out (6s, see BackendClient.defaultTimeout) — the chip
/// drops to a muted placeholder (a small lock glyph). No crash, no infinite
/// spinner. A later fetch (once the user allows the prompt) succeeds and the
/// real code appears.
struct TOTPCodeChip: View {
    let host: String
    @EnvironmentObject var appState: AppState

    // Successful fetch state.
    @State private var code: String?
    @State private var expiry: Date = .distantPast
    @State private var period: Int = 30

    // Lifecycle / presentation.
    @State private var failed = false
    @State private var loading = false
    @State private var justCopied = false
    @State private var copyResetTask: Task<Void, Never>?

    var body: some View {
        // 0.5s local tick drives the ring; we never poll the daemon faster
        // than this — re-fetch only happens at window rollover.
        TimelineView(.periodic(from: .now, by: 0.5)) { context in
            let fraction = fraction(at: context.date)
            content(fraction: fraction)
                .onChange(of: tick(context.date)) { _ in
                    // Window elapsed → fetch the next window's code. Guarded
                    // by `loading` so a slow fetch can't stack requests.
                    if code != nil, expiry.timeIntervalSinceNow <= 0, !loading {
                        Task { await fetch() }
                    }
                }
        }
        .task { await fetch() }
        .onDisappear { copyResetTask?.cancel() }
    }

    // MARK: - Content

    @ViewBuilder
    private func content(fraction: Double) -> some View {
        if let code, !failed {
            Button {
                copy(code)
            } label: {
                HStack(spacing: Spacing.xs) {
                    countdownRing(fraction: fraction)
                    if justCopied {
                        Image(systemName: "checkmark")
                            .font(.system(.callout, design: .monospaced).weight(.semibold))
                            .foregroundStyle(.green)
                            .transition(.opacity)
                    } else {
                        Text(grouped(code))
                            .font(.system(.callout, design: .monospaced))
                            .kerning(0.5)
                            .foregroundStyle(.primary)
                            .monospacedDigit()
                            .transition(.opacity)
                    }
                }
            }
            .buttonStyle(.plain)
            .help("Tap to copy 2FA code")
        } else {
            // Muted placeholder — failure (no secret / pending Keychain
            // prompt / error) or pre-first-fetch. Quiet lock glyph; never a
            // spinner-forever.
            Image(systemName: "lock")
                .font(.system(.caption, design: .rounded))
                .foregroundStyle(.tertiary)
                .help(failed ? "2FA code unavailable" : "Loading 2FA code…")
        }
    }

    private func countdownRing(fraction: Double) -> some View {
        let last10 = expiry.timeIntervalSinceNow <= 10
        return Circle()
            .trim(from: 0, to: max(0.0001, fraction))
            .stroke(ringColor, style: StrokeStyle(lineWidth: 2, lineCap: .round))
            .rotationEffect(.degrees(-90))
            .frame(width: 15, height: 15)
            // Subtle "breathing" in the final stretch of the window.
            .opacity(last10 ? breathingOpacity(fraction: fraction) : 1.0)
            .scaleEffect(last10 ? breathingScale(fraction: fraction) : 1.0)
            .animation(.easeInOut(duration: 0.5), value: fraction)
    }

    // MARK: - Colour / breathing

    private var ringColor: Color {
        let remaining = expiry.timeIntervalSinceNow
        if remaining <= 5 { return .red }
        if remaining <= 10 { return .orange }
        return .green
    }

    /// Gentle pulse — maps the sub-second phase to a soft opacity wobble.
    private func breathingOpacity(fraction: Double) -> Double {
        0.7 + 0.3 * (0.5 + 0.5 * sin(Date().timeIntervalSinceReferenceDate * 3))
    }

    private func breathingScale(fraction: Double) -> Double {
        1.0 + 0.06 * (0.5 + 0.5 * sin(Date().timeIntervalSinceReferenceDate * 3))
    }

    // MARK: - Ring math

    private func fraction(at date: Date) -> Double {
        guard period > 0 else { return 0 }
        let remaining = expiry.timeIntervalSince(date)
        return max(0, min(1, remaining / Double(period)))
    }

    /// Coarse integer "tick" used only as an onChange trigger so we evaluate
    /// the rollover condition without spamming work every render.
    private func tick(_ date: Date) -> Int {
        Int(date.timeIntervalSince1970 * 2)
    }

    // MARK: - Code formatting

    /// "832194" → "832 194" (two monospaced groups of three). Falls back to
    /// the raw string for non-6-digit codes.
    private func grouped(_ raw: String) -> String {
        guard raw.count == 6 else { return raw }
        let mid = raw.index(raw.startIndex, offsetBy: 3)
        return "\(raw[raw.startIndex..<mid]) \(raw[mid...])"
    }

    // MARK: - Fetch

    private func fetch() async {
        guard !loading else { return }
        loading = true
        defer { loading = false }
        do {
            let r = try await appState.hostTOTP(host)
            withAnimation(.easeInOut(duration: 0.2)) {
                self.code = r.code
                self.period = r.period > 0 ? r.period : 30
                self.expiry = Date().addingTimeInterval(Double(max(1, r.seconds_remaining)))
                self.failed = false
            }
        } catch {
            // No secret, daemon error, or a pending/denied Keychain prompt
            // (which times out at 6s). Show the muted state; a later fetch
            // after the user allows the prompt will succeed.
            withAnimation(.easeInOut(duration: 0.2)) {
                self.failed = true
            }
        }
    }

    // MARK: - Copy

    private func copy(_ code: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(code, forType: .string)
        appState.notchPresenter.show(
            systemImage: "doc.on.doc.fill",
            title: "Copied 2FA code",
            description: host,
            tint: .green
        )
        withAnimation(.easeInOut(duration: 0.15)) { justCopied = true }
        copyResetTask?.cancel()
        copyResetTask = Task {
            try? await Task.sleep(nanoseconds: 1_200_000_000)
            await MainActor.run {
                withAnimation(.easeInOut(duration: 0.15)) { justCopied = false }
            }
        }
    }
}
