import XCTest

final class SSHConfigParserTests: XCTestCase {
    func testSingleHostWithHostNameAndUser() {
        let cfg = """
        Host login01
            HostName login.hpc.example.edu
            User alice
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "login01",
                                   hostName: "login.hpc.example.edu",
                                   user: "alice")])
    }

    func testMultiAliasOnOneHostLineBothInheritDetails() {
        let cfg = """
        Host fasrc fas
            HostName login.hpc.example.edu
            User u
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "fasrc", hostName: "login.hpc.example.edu", user: "u"),
                        ConfigHost(alias: "fas", hostName: "login.hpc.example.edu", user: "u")])
    }

    func testWildcardHostsAreSkipped() {
        let cfg = """
        Host *
            ServerAliveInterval 60
        Host gh *.edu
            HostName example.edu
        """
        // `Host *` contributes nothing; `Host gh *.edu` keeps only `gh`.
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "gh", hostName: "example.edu", user: nil)])
    }

    func testCommentsAndIndentationAndCaseTolerated() {
        let cfg = """
        # a comment
          host  Box   # trailing comment
            hostname  1.2.3.4
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "Box", hostName: "1.2.3.4", user: nil)])
    }

    func testEmptyInput() {
        XCTAssertEqual(SSHConfigParser.parse(""), [])
        XCTAssertEqual(SSHConfigParser.parse("\n\n# just a comment\n"), [])
    }

    func testHostWithNoDetails() {
        XCTAssertEqual(SSHConfigParser.parse("Host bare\n"),
                       [ConfigHost(alias: "bare", hostName: nil, user: nil)])
    }

    // MARK: - parseFull (patterns + Include/Match detection + CRLF)

    func testParseFullRecordsWildcardPatternsAndConcreteHosts() {
        let cfg = """
        Host gpu-*
            User u
        Host login
            HostName login.rc.edu
        """
        let r = SSHConfigParser.parseFull(cfg)
        XCTAssertEqual(r.hosts, [ConfigHost(alias: "login", hostName: "login.rc.edu", user: nil)])
        XCTAssertEqual(r.patterns, ["gpu-*"])
        XCTAssertFalse(r.incompleteView)
    }

    func testParseFullFlagsIncludeAsUnresolved() {
        // The pure text parse doesn't follow Include → view is incomplete.
        let r = SSHConfigParser.parseFull("Include ~/.ssh/config.d/*\nHost a\n")
        XCTAssertTrue(r.includeUnresolved)
        XCTAssertTrue(r.incompleteView)
        XCTAssertEqual(r.hosts, [ConfigHost(alias: "a", hostName: nil, user: nil)])
    }

    func testParseFullDetectsMatch() {
        let r = SSHConfigParser.parseFull("Match host bar\n    User u\n")
        XCTAssertTrue(r.hasMatch)
        XCTAssertTrue(r.incompleteView)
    }

    func testParseToleratesCRLF() {
        let cfg = "Host box\r\n    HostName 1.2.3.4\r\n"
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "box", hostName: "1.2.3.4", user: nil)])
    }

    func testParseAcceptsEqualsSeparator() {
        // ssh treats '=' (with or without surrounding spaces) like whitespace.
        let cfg = """
        Host=box
            HostName=1.2.3.4
            User = bob
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "box", hostName: "1.2.3.4", user: "bob")])
    }

    // MARK: - parseConfig: following Include directives (filesystem)

    private func tempDir() -> String {
        let d = NSTemporaryDirectory() + "ssh2fa-parser-" + UUID().uuidString
        try? FileManager.default.createDirectory(atPath: d, withIntermediateDirectories: true)
        return d
    }

    private func write(_ text: String, to path: String) {
        try? FileManager.default.createDirectory(
            atPath: (path as NSString).deletingLastPathComponent,
            withIntermediateDirectories: true)
        try? text.write(toFile: path, atomically: true, encoding: .utf8)
    }

    func testParseConfigFollowsRelativeIncludeGlob() {
        let dir = tempDir()
        write("Host top\n    HostName t.edu\nInclude config.d/*\n", to: dir + "/config")
        write("Host work\n    HostName w.edu\n", to: dir + "/config.d/work.conf")
        write("Host gpu-*\n    User u\nHost lab\n    HostName l.edu\n", to: dir + "/config.d/lab.conf")
        let r = SSHConfigParser.parseConfig(at: dir + "/config", configDir: dir)
        XCTAssertEqual(Set(r.hosts.map { $0.alias }), ["top", "work", "lab"])
        XCTAssertEqual(r.patterns, ["gpu-*"])           // pattern from an included file is collected
        XCTAssertFalse(r.incompleteView)                // everything resolved
    }

    func testParseConfigConcreteMissingIncludeIsUnresolved() {
        let dir = tempDir()
        write("Host a\nInclude does-not-exist.conf\n", to: dir + "/config")
        let r = SSHConfigParser.parseConfig(at: dir + "/config", configDir: dir)
        XCTAssertEqual(r.hosts.map { $0.alias }, ["a"])
        XCTAssertTrue(r.includeUnresolved)              // a concrete missing include = blind
    }

    func testParseConfigEmptyGlobIsNotUnresolved() {
        let dir = tempDir()
        write("Host a\nInclude config.d/*\n", to: dir + "/config")
        try? FileManager.default.createDirectory(atPath: dir + "/config.d", withIntermediateDirectories: true)
        let r = SSHConfigParser.parseConfig(at: dir + "/config", configDir: dir)
        XCTAssertEqual(r.hosts.map { $0.alias }, ["a"])
        XCTAssertFalse(r.incompleteView)                // a glob matching nothing is fine
    }

    func testParseConfigIncludeCycleTerminates() {
        let dir = tempDir()
        write("Host a\nInclude b.conf\n", to: dir + "/config")
        write("Host b\nInclude config\n", to: dir + "/b.conf")   // points back → cycle
        let r = SSHConfigParser.parseConfig(at: dir + "/config", configDir: dir)
        XCTAssertEqual(Set(r.hosts.map { $0.alias }), ["a", "b"])   // each parsed once, no hang
    }
}
