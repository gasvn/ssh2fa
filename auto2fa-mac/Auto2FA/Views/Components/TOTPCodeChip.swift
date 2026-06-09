import SwiftUI

/// Inline, authenticator-style live 2FA (TOTP) chip for a host row.
///
/// Shows the current 6-digit code as two monospaced groups of three
/// (`832 194`) next to a small countdown ring that drains over the 30s
/// window and shifts colour as time runs out. Tap to copy the code.
///
/// ON-DEMAND (tap to reveal): the chip does NOT fetch anything until the user
/// taps "reveal" for that specific host. Auto-fetching a code for every host on
/// appear caused a Keychain "Always Allow" prompt storm (the daemon reads each
/// host's secret, and an unsigned daemon binary re-prompts per item) — and the
/// pile of unanswered prompts even wedged the daemon. So: nothing is read until
/// you explicitly ask for one host's code → at most ONE prompt, on your terms.
///
/// REFRESH (only while revealed): fetch the code once per ~30s window via
/// `appState.hostTOTP(host)` storing an absolute `expiry`; a 0.5s `TimelineView`
/// tick animates the ring LOCALLY (no IPC per tick) and re-fetches only at
/// window rollover. Collapsing (or the row disappearing) stops all activity.
///
/// GRACEFUL FAILURE: if a fetch throws (no secret / daemon error / a pending or
/// denied Keychain prompt that times out at 6s) the chip shows a muted "code
/// unavailable" glyph — no crash, no infinite spinner.
struct TOTPCodeChip: View {
    let host: String
    @EnvironmentObject var appState: AppState

    // Reveal gate — nothing is fetched until the user taps to reveal.
    @State private var revealed = false

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
        Group {
            if revealed {
                // 0.5s local tick drives the ring; re-fetch only at rollover.
                TimelineView(.periodic(from: .now, by: 0.5)) { context in
                    let fraction = fraction(at: context.date)
                    content(fraction: fraction)
                        .onChange(of: tick(context.date)) {
                            if code != nil, expiry.timeIntervalSinceNow <= 0, !loading {
                                Task { await fetch() }
                            }
                        }
                }
                .task { await fetch() }   // first fetch happens only after reveal
            } else {
                revealButton
            }
        }
        .onDisappear {
            copyResetTask?.cancel()
            // Stop refreshing + drop the code when the row goes away.
            revealed = false
            code = nil
            failed = false
        }
    }

    /// Default state: a quiet "show code" affordance. No Keychain read until tapped.
    private var revealButton: some View {
        Button {
            revealed = true   // the revealed branch's .task does the (single) fetch
        } label: {
            HStack(spacing: Spacing.xs) {
                Image(systemName: "key.horizontal.fill")
                Text("•• ••")
                    .font(.system(.callout, design: .monospaced))
            }
            .foregroundStyle(.tertiary)
        }
        .buttonStyle(.plain)
        .help("Show 2FA code (reads this host's secret once)")
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
            // spinner-forever. TAPPABLE: after a failed first fetch (e.g. a
            // pending Keychain prompt) nothing else ever refetched — the
            // rollover refetch requires code != nil — so the chip was stuck
            // at the lock glyph until the row was recreated.
            Button {
                if !loading { Task { await fetch() } }
            } label: {
                Image(systemName: "lock")
                    .font(.system(.caption, design: .rounded))
                    .foregroundStyle(.tertiary)
            }
            .buttonStyle(.plain)
            .help(failed ? "2FA code unavailable — tap to retry" : "Loading 2FA code…")
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
