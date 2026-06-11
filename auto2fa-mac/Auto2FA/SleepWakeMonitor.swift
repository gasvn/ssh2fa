import AppKit

/// Subscribes to AppKit's workspace sleep/wake notifications and lets a caller
/// react to wake events. We need this because SSH ControlMaster's underlying
/// TCP connections die during system suspend but the local process stays
/// alive — `ssh -O check` still passes against the dead socket, so tunnels
/// silently break with no recovery. The fix is to tell the daemon to
/// rebuild masters and restart tunnels immediately on wake.
@MainActor
final class SleepWakeMonitor {
    private var willSleepObserver: NSObjectProtocol?
    private var didWakeObserver: NSObjectProtocol?
    private let onWake: () -> Void
    private let onSleep: (() -> Void)?

    init(onSleep: (() -> Void)? = nil, onWake: @escaping () -> Void) {
        self.onSleep = onSleep
        self.onWake = onWake
    }

    func start() {
        let nc = NSWorkspace.shared.notificationCenter
        // The observers fire on .main queue but are typed as @Sendable, so
        // we hop into a MainActor Task to read the actor-isolated callbacks.
        willSleepObserver = nc.addObserver(
            forName: NSWorkspace.willSleepNotification,
            object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                NSLog("[SSH2FA] system going to sleep")
                self?.onSleep?()
            }
        }
        didWakeObserver = nc.addObserver(
            forName: NSWorkspace.didWakeNotification,
            object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                NSLog("[SSH2FA] system woke from sleep")
                self?.onWake()
            }
        }
    }

    deinit {
        let nc = NSWorkspace.shared.notificationCenter
        if let o = willSleepObserver { nc.removeObserver(o) }
        if let o = didWakeObserver { nc.removeObserver(o) }
    }
}
