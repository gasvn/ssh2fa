import XCTest

final class SSHConfigParserTests: XCTestCase {
    func testSingleHostWithHostNameAndUser() {
        let cfg = """
        Host kempner
            HostName login.rc.fas.harvard.edu
            User shgao
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "kempner",
                                   hostName: "login.rc.fas.harvard.edu",
                                   user: "shgao")])
    }

    func testMultiAliasOnOneHostLineBothInheritDetails() {
        let cfg = """
        Host fasrc fas
            HostName boslogin.rc.fas.harvard.edu
            User u
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "fasrc", hostName: "boslogin.rc.fas.harvard.edu", user: "u"),
                        ConfigHost(alias: "fas", hostName: "boslogin.rc.fas.harvard.edu", user: "u")])
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
        XCTAssertFalse(r.hasIncludeOrMatch)
    }

    func testParseFullDetectsInclude() {
        let r = SSHConfigParser.parseFull("Include ~/.ssh/config.d/*\nHost a\n")
        XCTAssertTrue(r.hasIncludeOrMatch)
        XCTAssertEqual(r.hosts, [ConfigHost(alias: "a", hostName: nil, user: nil)])
    }

    func testParseFullDetectsMatch() {
        XCTAssertTrue(SSHConfigParser.parseFull("Match host bar\n    User u\n").hasIncludeOrMatch)
    }

    func testParseToleratesCRLF() {
        let cfg = "Host box\r\n    HostName 1.2.3.4\r\n"
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "box", hostName: "1.2.3.4", user: nil)])
    }
}
