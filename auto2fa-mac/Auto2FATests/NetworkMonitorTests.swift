import XCTest

// NetworkMonitor is compiled into this test bundle via project.yml. We test only
// the PURE signature builder — the part that decides whether the network
// identity changed. (NWPathMonitor itself can't be driven headlessly.)
final class NetworkMonitorTests: XCTestCase {

    private func snapshot(status: String = "satisfied",
                          primary: String = "wifi",
                          addresses: [String] = ["en0=192.168.1.5"],
                          other: Bool = false) -> NetworkMonitor.Snapshot {
        NetworkMonitor.Snapshot(statusKey: status,
                                primary: primary,
                                addresses: addresses,
                                routesOverOtherInterface: other)
    }

    func testSameInputsProduceSameSignature() {
        let a = NetworkMonitor.makeSignature(statusKey: "satisfied", primary: "wifi",
                                             addresses: ["en0=192.168.1.5"])
        let b = NetworkMonitor.makeSignature(statusKey: "satisfied", primary: "wifi",
                                             addresses: ["en0=192.168.1.5"])
        XCTAssertEqual(a, b)
    }

    func testWifiToWifiSwitchChangesSignature() {
        // The bug: switching between two Wi-Fi networks keeps status+type the
        // same ("satisfied|wifi"), so the OLD signature never changed and
        // recovery never fired. Including the interface IP must make these two
        // distinct so the switch is detected.
        let home = NetworkMonitor.makeSignature(statusKey: "satisfied", primary: "wifi",
                                                addresses: ["en0=192.168.1.5"])
        let cafe = NetworkMonitor.makeSignature(statusKey: "satisfied", primary: "wifi",
                                                addresses: ["en0=10.0.0.9"])
        XCTAssertNotEqual(home, cafe)
    }

    func testStatusChangeChangesSignature() {
        let up = NetworkMonitor.makeSignature(statusKey: "satisfied", primary: "wifi",
                                              addresses: ["en0=192.168.1.5"])
        let down = NetworkMonitor.makeSignature(statusKey: "unsatisfied", primary: "wifi",
                                                addresses: [])
        XCTAssertNotEqual(up, down)
    }

    func testGainingAVpnAddressChangesSignature() {
        let plain = NetworkMonitor.makeSignature(statusKey: "satisfied", primary: "wifi",
                                                 addresses: ["en0=192.168.1.5"])
        let vpn = NetworkMonitor.makeSignature(statusKey: "satisfied", primary: "wifi",
                                               addresses: ["en0=192.168.1.5", "utun3=10.8.0.2"])
        XCTAssertNotEqual(plain, vpn)
    }

    func testTemporaryPathLossOnlyProbesAndNeverForces() {
        let previous = snapshot()
        let current = snapshot()
        XCTAssertEqual(
            NetworkMonitor.recoveryDecision(previousStable: previous,
                                              current: current,
                                              sawUnavailable: true),
            .probe
        )
    }

    func testVpnRouteChangeOnlyProbes() {
        XCTAssertEqual(
            NetworkMonitor.recoveryDecision(previousStable: snapshot(other: false),
                                              current: snapshot(other: true),
                                              sawUnavailable: false),
            .probe
        )
    }

    func testConfirmedPhysicalAddressChangeForcesVerifiedRecovery() {
        XCTAssertEqual(
            NetworkMonitor.recoveryDecision(
                previousStable: snapshot(addresses: ["en0=192.168.1.5"]),
                current: snapshot(addresses: ["en0=10.0.0.9"]),
                sawUnavailable: false
            ),
            .force
        )
    }

    func testMissingAddressCanNeverForce() {
        XCTAssertEqual(
            NetworkMonitor.recoveryDecision(previousStable: snapshot(),
                                              current: snapshot(addresses: []),
                                              sawUnavailable: false),
            .none
        )
    }

    func testUnsatisfiedCandidateDoesNothingUntilNetworkReturns() {
        XCTAssertEqual(
            NetworkMonitor.recoveryDecision(previousStable: snapshot(),
                                              current: snapshot(status: "unsatisfied",
                                                                primary: "other",
                                                                addresses: []),
                                              sawUnavailable: true),
            .none
        )
    }
}
