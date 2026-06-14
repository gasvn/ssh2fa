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
}
