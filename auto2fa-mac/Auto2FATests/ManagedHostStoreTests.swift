import XCTest

final class ManagedHostStoreTests: XCTestCase {
    private func tmp() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("mhs-\(UUID().uuidString)")
            .appendingPathComponent("managed_hosts.json")
    }

    func testMissingFileLoadsEmpty() {
        XCTAssertTrue(ManagedHostStore.load(from: tmp()).isEmpty)
    }

    func testUpsertRoundTrips() throws {
        let url = tmp()
        let c = ManagedHostConn(alias: "cluster01", hostName: "login.hpc.example.edu",
                                user: "jdoe", port: 22)
        try ManagedHostStore.upsert(c, in: url)
        XCTAssertEqual(ManagedHostStore.load(from: url), [c])
    }

    func testUpsertReplacesByAlias() throws {
        let url = tmp()
        try ManagedHostStore.upsert(ManagedHostConn(alias: "a", hostName: "h1", user: "u", port: 22), in: url)
        try ManagedHostStore.upsert(ManagedHostConn(alias: "a", hostName: "h2", user: "u", port: 22), in: url)
        let back = ManagedHostStore.load(from: url)
        XCTAssertEqual(back.count, 1)
        XCTAssertEqual(back.first?.hostName, "h2")
    }

    func testRemove() throws {
        let url = tmp()
        try ManagedHostStore.upsert(ManagedHostConn(alias: "a", hostName: "h", user: "u", port: 22), in: url)
        try ManagedHostStore.remove(alias: "a", in: url)
        XCTAssertTrue(ManagedHostStore.load(from: url).isEmpty)
    }
}
