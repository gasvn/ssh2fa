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
    private let queue = DispatchQueue(label: "com.ssh2fa.networkmonitor")
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

    /// Pure signature builder — kept separate so the "did the network identity
    /// change?" decision is unit-tested without a live NWPathMonitor.
    nonisolated static func makeSignature(statusKey: String, primary: String, addresses: [String]) -> String {
        "\(statusKey)|\(primary)|\(addresses.joined(separator: ","))"
    }

    /// IPv4 addresses of the REAL connectivity interfaces in this path (Wi-Fi /
    /// Ethernet / cellular). Switching between two Wi-Fi networks keeps the
    /// interface type "wifi" but changes en0's IP, so the IP is what makes the
    /// switch detectable. Docker bridges and VPN utuns are type `.other` and are
    /// deliberately excluded so they don't spuriously trip recovery — though
    /// over-firing is now cheap anyway: the daemon only force-rebuilds masters
    /// whose connection is genuinely dead.
    nonisolated static func physicalIPv4Addresses(path: NWPath) -> [String] {
        let names = Set(path.availableInterfaces
            .filter { $0.type == .wifi || $0.type == .wiredEthernet || $0.type == .cellular }
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
            if r == 0 { out.append("\(name)=\(String(cString: host))") }
        }
        return out.sorted()
    }

    private func handle(path: NWPath) {
        // Signature = path status + primary interface TYPE + the IPv4 addresses
        // of the real connectivity interfaces. The address component is what
        // catches a Wi-Fi→Wi-Fi switch (same type/status, different IP) that the
        // old type-only signature missed — the reason ssh masters stayed dead
        // with no recovery fired.
        let primary: String
        if path.usesInterfaceType(.wifi) { primary = "wifi" }
        else if path.usesInterfaceType(.wiredEthernet) { primary = "eth" }
        else if path.usesInterfaceType(.cellular) { primary = "cell" }
        else if path.usesInterfaceType(.loopback) { primary = "lo" }
        else { primary = "other" }
        let signature = Self.makeSignature(statusKey: "\(path.status)", primary: primary,
                                           addresses: Self.physicalIPv4Addresses(path: path))
        guard signature != lastInterfaceSignature else { return }
        let prev = lastInterfaceSignature
        lastInterfaceSignature = signature
        NSLog("[SSH2FA] network change: \(prev) → \(signature)")
        guard !prev.isEmpty else { return }

        // Debounce: rapid changes (e.g. interface dropping then coming back
        // when switching Wi-Fi) collapse into one fire.
        pendingFireTask?.cancel()
        pendingFireTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64((self?.debounce ?? 3.0) * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await MainActor.run {
                NSLog("[SSH2FA] network change settled — firing recovery")
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
