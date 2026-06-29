import Foundation

/// The "Get started" steps a new user works through. Pure + Foundation-only so
/// it compiles into the headless test bundle (like SearchFilter / SlurmTime).
enum OnboardingStep: CaseIterable {
    case addHost       // registered at least one host
    case seeConnect    // a host reached the connected state
    case openTerminal  // used a host's Terminal action at least once
}

enum OnboardingChecklist {
    /// Which steps are complete given the live signals. `warmReuse` also
    /// satisfies the "open a terminal" step: a user who turned on warm-reuse is
    /// running `ssh <host>` in their OWN terminal (skipping 2FA) — the app's goal
    /// — so the step shouldn't pin forever just because they never clicked the
    /// in-app "Open Terminal".
    static func completed(hostCount: Int, anyConnected: Bool, usedTerminal: Bool,
                          warmReuse: Bool = false) -> Set<OnboardingStep> {
        var done: Set<OnboardingStep> = []
        if hostCount > 0 { done.insert(.addHost) }
        if anyConnected { done.insert(.seeConnect) }
        if usedTerminal || warmReuse { done.insert(.openTerminal) }
        return done
    }

    /// Show the checklist until every step is done (then the user knows the flow).
    static func shouldShow(hostCount: Int, anyConnected: Bool, usedTerminal: Bool,
                           warmReuse: Bool = false) -> Bool {
        completed(hostCount: hostCount, anyConnected: anyConnected,
                  usedTerminal: usedTerminal, warmReuse: warmReuse).count
            < OnboardingStep.allCases.count
    }
}
