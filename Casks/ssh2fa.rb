cask "ssh2fa" do
  version "1.0.0"
  # Set per release:  shasum -a 256 dist/SSH2FA.dmg
  # (package-app.sh prints this; the release step pastes it here.)
  sha256 "89aaab99196404a20fc6c3383ec6f55cc5df2bd354d433addb2bd24e255ec1d3"

  url "https://github.com/gasvn/ssh2fa/releases/download/v#{version}/SSH2FA.dmg"
  name "SSH2FA"
  desc "Keeps SSH ControlMaster pools warm and auto-answers Duo/TOTP 2FA logins"
  homepage "https://github.com/gasvn/ssh2fa"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: :sonoma

  app "SSH2FA.app"

  # The daemon lives INSIDE the app bundle (run in place); removing the app
  # removes it. We only need to quit the app and unload its LaunchAgent.
  uninstall launchctl: "com.ssh2fa.daemon",
            quit:      "com.ssh2fa.app"

  # `brew uninstall --zap` wipes everything, including saved hosts + the
  # Keychain credentials SSH2FA stored (service "auto2fa").
  zap script: {
        executable: "/bin/sh",
        args:       ["-c", "while /usr/bin/security delete-generic-password -s auto2fa >/dev/null 2>&1; do :; done"],
      },
      trash:  [
        "~/.ssh2fa",
        "~/Library/Caches/com.ssh2fa.app",
        "~/Library/HTTPStorages/com.ssh2fa.app",
        "~/Library/LaunchAgents/com.ssh2fa.daemon.plist",
        "~/Library/Preferences/com.ssh2fa.app.plist",
      ]

  caveats <<~EOS
    SSH2FA installs a background helper (LaunchAgent com.ssh2fa.daemon) on
    first launch. If the build isn't notarized, macOS Gatekeeper blocks the
    first launch — open it once via System Settings → Privacy & Security →
    "Open Anyway".

    To remove EVERYTHING (saved hosts + Keychain credentials), run:
      brew uninstall --zap --cask ssh2fa
  EOS
end
