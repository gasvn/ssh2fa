import Foundation
import SwiftUI
import AppKit
import LocalAuthentication

/// Optional biometric gate for the app's private windows. Uses
/// `deviceOwnerAuthentication` (Touch ID with a Mac-password fallback), which
/// needs NO entitlement on this non-sandboxed app.
@MainActor
final class BiometricLock: ObservableObject {
    /// When the user last authenticated successfully — drives the grace window.
    @Published var lastSuccessfulAuth: Date?

    /// Seconds after a success during which re-opening a gated window does NOT
    /// re-prompt ("re-lock on close, with a grace period").
    static let graceInterval: TimeInterval = 60

    var enabled: Bool { UserDefaults.standard.bool(forKey: SettingsKey.requireTouchID) }

    func shouldChallengeNow() -> Bool {
        LockCore.shouldChallenge(enabled: enabled, lastAuth: lastSuccessfulAuth,
                                 now: Date(), grace: BiometricLock.graceInterval)
    }

    /// Can the device evaluate owner auth at all (biometrics OR a login password)?
    static func availability() -> (ok: Bool, reason: String?) {
        let ctx = LAContext()
        var err: NSError?
        let ok = ctx.canEvaluatePolicy(.deviceOwnerAuthentication, error: &err)
        return (ok, err?.localizedDescription)
    }

    /// Prompt for auth. A FRESH LAContext per call (reuse caches a prior result).
    func authenticate() async -> Bool {
        let ctx = LAContext()
        let ok: Bool = await withCheckedContinuation { cont in
            ctx.evaluatePolicy(.deviceOwnerAuthentication,
                               localizedReason: "Unlock SSH2FA") { success, _ in
                cont.resume(returning: success)
            }
        }
        if ok { lastSuccessfulAuth = Date() }
        return ok
    }
}

/// Wraps a window's content; when the lock is engaged it shows `LockedView` and
/// requires auth before revealing `content`. Re-evaluates on appear and when the
/// app becomes active, so an unattended open window re-locks after the grace.
struct LockGate<Content: View>: View {
    @EnvironmentObject private var lock: BiometricLock
    @Environment(\.scenePhase) private var scenePhase
    // Observe the setting itself so flipping "Require Touch ID" takes effect
    // immediately — without this, toggling it on while a window was already open
    // did nothing (the gate only re-checked on appear/active), which read as
    // "the toggle has no effect".
    @AppStorage(SettingsKey.requireTouchID) private var requireTouchID = false
    @State private var unlocked = false
    @State private var authing = false
    @State private var authFailed = false
    @ViewBuilder var content: () -> Content

    var body: some View {
        Group {
            if unlocked {
                content()
            } else {
                LockedView(authing: authing, failed: authFailed) { Task { await attempt() } }
            }
        }
        .onAppear { evaluate() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active {
                evaluate()
            } else if lock.enabled {
                // Re-lock as the window leaves the foreground so coming back
                // re-challenges (subject to the grace window) — this is the
                // "re-locks after you close the window" behavior.
                unlocked = false
            }
        }
        .onChange(of: requireTouchID) { _, _ in evaluate() }
        .onChange(of: lock.lastSuccessfulAuth) { _, _ in evaluate() }
    }

    private func evaluate() {
        if !lock.shouldChallengeNow() {
            unlocked = true
        } else if !BiometricLock.availability().ok {
            // Fail OPEN — never trap the user out of their own app when neither
            // biometrics nor a login password can satisfy the policy.
            unlocked = true
        } else {
            unlocked = false
            Task { await attempt() }
        }
    }

    private func attempt() async {
        guard !authing else { return }
        authing = true
        authFailed = false
        let ok = await lock.authenticate()
        authing = false
        if ok { unlocked = true } else { authFailed = true }
    }
}

struct LockedView: View {
    let authing: Bool
    var failed: Bool = false
    let unlock: () -> Void
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "lock.fill")
                .font(.system(size: 40)).foregroundStyle(.secondary)
            Text("SSH2FA is locked").font(.title3)
            if failed && !authing {
                Text("Authentication was cancelled or failed. Try again, or quit.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            Button(authing ? "Authenticating…" : "Unlock", action: unlock)
                .controlSize(.large)
                .disabled(authing)
            // An escape hatch: if biometrics keep failing, the only other way
            // out used to be the menu bar. Let the user quit from here.
            Button("Quit SSH2FA") { NSApplication.shared.terminate(nil) }
                .controlSize(.small)
                .buttonStyle(.link)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }
}
