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
    struct Snapshot: Equatable {
        let statusKey: String
        let primary: String
        let addresses: [String]
        let routesOverOtherInterface: Bool

        var isSatisfied: Bool { statusKey == "satisfied" }
        var hasPhysicalIdentity: Bool {
            isSatisfied && primary != "other" && primary != "lo" && !addresses.isEmpty
        }
        var physicalIdentity: String { "\(primary)|\(addresses.joined(separator: ","))" }
    }

    enum RecoveryDecision: Equatable {
        case none
        case probe
        case force
    }

    private let monitor = NWPathMonitor()
    private let queue = DispatchQueue(label: "com.ssh2fa.networkmonitor")
    private var lastObservedSnapshot: Snapshot?
    private var lastStableSnapshot: Snapshot?
    private var sawUnavailableSinceStable = false
    private var pendingFireTask: Task<Void, Never>?
    private let onChange: (_ force: Bool) -> Void

    /// Coalesce window — wait this long after the last path update before
    /// actually firing onChange.
    private let debounce: TimeInterval = 3.0

    init(onChange: @escaping (_ force: Bool) -> Void) {
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

    /// Pure signature builder — kept separate so the "did the network identity
    /// change?" decision is unit-tested without a live NWPathMonitor.
    nonisolated static func makeSignature(statusKey: String, primary: String, addresses: [String]) -> String {
        "\(statusKey)|\(primary)|\(addresses.joined(separator: ","))"
    }

    /// Decide how aggressively a settled path change should be handled.
    ///
    /// `force` is intentionally narrow: both the old and new paths must have a
    /// concrete physical identity and that identity must differ. A temporary
    /// NWPath status/VPN-route notification is only a request to probe. The old
    /// implementation sent `force=true` for *every* signature change, so a
    /// transient `.unsatisfied` notification tore down every healthy master.
    nonisolated static func recoveryDecision(previousStable: Snapshot?,
                                              current: Snapshot,
                                              sawUnavailable: Bool) -> RecoveryDecision {
        guard current.isSatisfied, let previousStable else { return .none }
        if previousStable.hasPhysicalIdentity,
           current.hasPhysicalIdentity,
           previousStable.physicalIdentity != current.physicalIdentity {
            return .force
        }
        if sawUnavailable
            || previousStable.routesOverOtherInterface != current.routesOverOtherInterface {
            return .probe
        }
        return .none
    }

    /// IPv4 addresses of the REAL connectivity interfaces in this path (Wi-Fi /
    /// Ethernet / cellular). Switching between two Wi-Fi networks keeps the
    /// interface type "wifi" but changes en0's IP, so the IP is what makes the
    /// switch detectable. Docker bridges and VPN utuns are type `.other` and are
    /// deliberately excluded so they don't spuriously trip recovery — though
    /// over-firing is now cheap anyway: the daemon only force-rebuilds masters
    /// whose connection is genuinely dead.
    nonisolated static func physicalIPv4Addresses(path: NWPath) -> [String] {
        let primaryType: NWInterface.InterfaceType?
        if path.usesInterfaceType(.wifi) { primaryType = .wifi }
        else if path.usesInterfaceType(.wiredEthernet) { primaryType = .wiredEthernet }
        else if path.usesInterfaceType(.cellular) { primaryType = .cellular }
        else { primaryType = nil }
        guard let primaryType else { return [] }

        // `availableInterfaces` includes interfaces that are present but are
        // not carrying this path. Including all of them made an unrelated
        // Ethernet/bridge address churn look like the active network changed.
        let names = Set(path.availableInterfaces
            .filter { $0.type == primaryType }
            .map { $0.name })
        guard !names.isEmpty else { return [] }

        var out: [String] = []
        var ifap: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifap) == 0 else { return [] }
        defer { freeifaddrs(ifap) }
        var ptr = ifap
        while let cur = ptr {
            defer { ptr = cur.pointee.ifa_next }
            let ifa = cur.pointee
            guard let addr = ifa.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET) else { continue }
            let name = String(cString: ifa.ifa_name)
            guard names.contains(name) else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            let r = getnameinfo(addr, socklen_t(addr.pointee.sa_len),
                                &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST)
            if r == 0 {
                let ip = String(cString: host)
                // A self-assigned link-local address is a settling artifact,
                // not a usable network identity. Never force-rebuild on it.
                if !ip.hasPrefix("169.254.") && ip != "0.0.0.0" {
                    out.append("\(name)=\(ip)")
                }
            }
        }
        return out.sorted()
    }

    nonisolated static func snapshot(path: NWPath) -> Snapshot {
        let primary: String
        if path.usesInterfaceType(.wifi) { primary = "wifi" }
        else if path.usesInterfaceType(.wiredEthernet) { primary = "eth" }
        else if path.usesInterfaceType(.cellular) { primary = "cell" }
        else if path.usesInterfaceType(.loopback) { primary = "lo" }
        else { primary = "other" }
        return Snapshot(
            statusKey: "\(path.status)",
            primary: primary,
            addresses: physicalIPv4Addresses(path: path),
            // VPN/route changes can invalidate traffic without changing the
            // physical address. They merit a probe, never a blind teardown.
            routesOverOtherInterface: path.usesInterfaceType(.other)
        )
    }

    private func handle(path: NWPath) {
        let snapshot = Self.snapshot(path: path)
        guard snapshot != lastObservedSnapshot else { return }
        let previous = lastObservedSnapshot
        lastObservedSnapshot = snapshot
        NSLog("[SSH2FA] network change: \(String(describing: previous)) → \(snapshot)")

        // Establish the first trustworthy baseline without reconnecting.
        guard previous != nil else {
            if snapshot.hasPhysicalIdentity { lastStableSnapshot = snapshot }
            return
        }
        if !snapshot.isSatisfied { sawUnavailableSinceStable = true }

        // Debounce: rapid changes (e.g. interface dropping then coming back
        // when switching Wi-Fi) collapse into one fire.
        pendingFireTask?.cancel()
        pendingFireTask = Task { [weak self, path] in
            try? await Task.sleep(nanoseconds: UInt64((self?.debounce ?? 3.0) * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.settle(path: path)
        }
    }

    private func settle(path: NWPath) {
        // Re-sample getifaddrs after the quiet period. A callback captured while
        // DHCP was between addresses must not become a forced identity change.
        let current = Self.snapshot(path: path)
        guard current == lastObservedSnapshot else {
            handle(path: path)
            return
        }
        guard current.isSatisfied else { return } // wait for the up transition

        let decision = Self.recoveryDecision(
            previousStable: lastStableSnapshot,
            current: current,
            sawUnavailable: sawUnavailableSinceStable
        )
        if current.hasPhysicalIdentity { lastStableSnapshot = current }
        sawUnavailableSinceStable = false

        switch decision {
        case .none:
            return
        case .probe:
            NSLog("[SSH2FA] network change settled — probing existing connections")
            onChange(false)
        case .force:
            NSLog("[SSH2FA] physical network identity changed — requesting verified recovery")
            onChange(true)
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
