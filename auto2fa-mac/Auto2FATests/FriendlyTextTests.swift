import XCTest

/// Regression tests for the direct-mode status copy: an idle DIRECT tunnel must
/// never be told to "Pick a compute node to start" (it has no node), while the
/// compute path keeps its original behavior.
final class FriendlyTextTests: XCTestCase {

    /// Decode a Tunnel from a daemon-style snapshot. `directHost`/`lastNode`
    /// default to nil; `status`/`lastMsg` are caller-controlled.
    private func makeTunnel(status: String,
                            directHost: String? = nil,
                            lastNode: String? = nil,
                            lastMsg: String = "") throws -> Tunnel {
        func q(_ s: String?) -> String { s.map { "\"\($0)\"" } ?? "null" }
        let json = """
        {
          "name": "web", "local_port": 9000, "remote_port": 9000,
          "jump_candidates": null, "last_node": \(q(lastNode)),
          "last_user": null, "direct_host": \(q(directHost)),
          "auto_start": false, "post_connect_cmd": null, "tags": [],
          "url_path": null, "active_jump": null, "status": "\(status)",
          "last_msg": "\(lastMsg)", "last_alive_at": 0.0,
          "total_uptime_sec": 0.0, "connect_count": 0, "fail_count": 0
        }
        """
        return try JSONDecoder().decode(Tunnel.self, from: Data(json.utf8))
    }

    func testDirectFlagDecodes() throws {
        let direct = try makeTunnel(status: "idle", directHost: "loginhost")
        XCTAssertTrue(direct.isDirect)
        XCTAssertEqual(direct.directHost, "loginhost")
        let compute = try makeTunnel(status: "idle")
        XCTAssertFalse(compute.isDirect)
    }

    /// The bug: an idle direct tunnel showed "Pick a compute node to start".
    /// It must instead surface the daemon message, or plain "Idle".
    func testIdleDirectTunnelDoesNotSayPickNode() throws {
        let parked = try makeTunnel(status: "idle", directHost: "loginhost",
                                    lastMsg: "waiting for host loginhost")
        let blurb = FriendlyText.tunnelStatusBlurb(parked)
        XCTAssertEqual(blurb, "waiting for host loginhost")
        XCTAssertFalse(blurb.contains("compute node"))
    }

    func testIdleDirectTunnelEmptyMessageIsIdle() throws {
        let t = try makeTunnel(status: "idle", directHost: "loginhost", lastMsg: "")
        XCTAssertEqual(FriendlyText.tunnelStatusBlurb(t), "Idle")
    }

    /// Compute path unchanged: an idle compute tunnel with no node still gets
    /// the "pick a node" nudge.
    func testIdleComputeTunnelNoNodeStillSaysPickNode() throws {
        let t = try makeTunnel(status: "idle", directHost: nil, lastNode: nil)
        XCTAssertEqual(FriendlyText.tunnelStatusBlurb(t), "Pick a compute node to start")
    }

    func testIdleComputeTunnelWithNodeIsIdle() throws {
        let t = try makeTunnel(status: "idle", directHost: nil, lastNode: "holygpu01")
        XCTAssertEqual(FriendlyText.tunnelStatusBlurb(t), "Idle")
    }
}
