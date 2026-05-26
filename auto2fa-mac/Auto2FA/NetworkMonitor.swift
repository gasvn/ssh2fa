import Foundation
import Network

/// Detect Wi-Fi / Ethernet / VPN changes and fire onChange.
/// Mac sleep/wake recovery already exists; this is the sibling that catches
/// "I switched Wi-Fi at the coffee shop" or "VPN connected/disconnected" —
/// both of which silently kill every SSH ControlMaster's underlying TCP.
///
/// Uses Network.framework's NWPathMonitor (macOS 10.14+). We coalesce
/// notifications: many changes can fire in rapid succession during a
/// network switch, and we only want one wake_recover trigger per ~3s
/// quiet period.
@MainActor
final class NetworkMonitor {
    private let monitor = NWPathMonitor()
    private let queue = DispatchQueue(label: "com.auto2fa.networkmonitor")
    private var lastInterfaceSignature: String = ""
    private var pendingFireTask: Task<Void, Never>?
    private let onChange: () -> Void

    /// Coalesce window — wait this long after the last path update before
    /// actually firing onChange.
    private let debounce: TimeInterval = 3.0

    init(onChange: @escaping () -> Void) {
        self.onChange = onChange
    }

    func start() {
        monitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor [weak self] in
                self?.handle(path: path)
            }
        }
        monitor.start(queue: queue)
    }

    func stop() {
        monitor.cancel()
        pendingFireTask?.cancel()
    }

    private func handle(path: NWPath) {
        // Build a signature using ONLY the primary connectivity interface
        // type (wifi vs ethernet vs cellular) + path status. Docker spinning
        // up a bridge interface or VPN flapping a utun would otherwise
        // mutate `availableInterfaces` and falsely trigger a recovery
        // every few seconds.
        let primary: String
        if path.usesInterfaceType(.wifi) { primary = "wifi" }
        else if path.usesInterfaceType(.wiredEthernet) { primary = "eth" }
        else if path.usesInterfaceType(.cellular) { primary = "cell" }
        else if path.usesInterfaceType(.loopback) { primary = "lo" }
        else { primary = "other" }
        let signature = "\(path.status)|\(primary)"
        guard signature != lastInterfaceSignature else { return }
        let prev = lastInterfaceSignature
        lastInterfaceSignature = signature
        NSLog("[Auto2FA] network change: \(prev) → \(signature)")
        guard !prev.isEmpty else { return }

        // Debounce: rapid changes (e.g. interface dropping then coming back
        // when switching Wi-Fi) collapse into one fire.
        pendingFireTask?.cancel()
        pendingFireTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64((self?.debounce ?? 3.0) * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await MainActor.run {
                NSLog("[Auto2FA] network change settled — firing recovery")
                self?.onChange()
            }
        }
    }
}

private extension NWInterface.InterfaceType {
    var debug: String {
        switch self {
        case .wifi: return "wifi"
        case .cellular: return "cell"
        case .wiredEthernet: return "eth"
        case .loopback: return "lo"
        case .other: return "other"
        @unknown default: return "?"
        }
    }
}
