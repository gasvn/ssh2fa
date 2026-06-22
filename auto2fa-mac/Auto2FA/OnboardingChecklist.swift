import Foundation

/// The "Get started" steps a new user works through. Pure + Foundation-only so
/// it compiles into the headless test bundle (like SearchFilter / SlurmTime).
enum OnboardingStep: CaseIterable {
    case addHost       // registered at least one host
    case seeConnect    // a host reached the connected state
    case openTerminal  // used a host's Terminal action at least once
}

enum OnboardingChecklist {
    /// Which steps are complete given the live signals.
    static func completed(hostCount: Int, anyConnected: Bool, usedTerminal: Bool) -> Set<OnboardingStep> {
        var done: Set<OnboardingStep> = []
        if hostCount > 0 { done.insert(.addHost) }
        if anyConnected { done.insert(.seeConnect) }
        if usedTerminal { done.insert(.openTerminal) }
        return done
    }

    /// Show the checklist until every step is done (then the user knows the flow).
    static func shouldShow(hostCount: Int, anyConnected: Bool, usedTerminal: Bool) -> Bool {
        completed(hostCount: hostCount, anyConnected: anyConnected, usedTerminal: usedTerminal).count
            < OnboardingStep.allCases.count
    }
}
