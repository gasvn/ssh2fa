cask "auto2fa" do
  version "1.0.0"
  # Set per release:  shasum -a 256 dist/Auto2FA.dmg
  # (package-app.sh prints this; the release step pastes it here.)
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/gasvn/auto2fa/releases/download/v#{version}/Auto2FA.dmg",
      verified: "github.com/gasvn/auto2fa/"
  name "Auto2FA"
  desc "Keeps SSH ControlMaster pools warm and auto-answers Duo/TOTP 2FA logins"
  homepage "https://github.com/gasvn/auto2fa"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: :sonoma

  app "Auto2FA.app"

  # The daemon lives INSIDE the app bundle (run in place); removing the app
  # removes it. We only need to quit the app and unload its LaunchAgent.
  uninstall launchctl: "com.auto2fa.daemon",
            quit:      "com.auto2fa.app"

  # `brew uninstall --zap` wipes everything, including saved hosts + the
  # Keychain credentials Auto2FA stored (service "auto2fa").
  zap script: {
        executable: "/bin/sh",
        args:       ["-c", "while /usr/bin/security delete-generic-password -s auto2fa >/dev/null 2>&1; do :; done"],
      },
      trash:  [
        "~/.auto2fa",
        "~/Library/Caches/com.auto2fa.app",
        "~/Library/HTTPStorages/com.auto2fa.app",
        "~/Library/LaunchAgents/com.auto2fa.daemon.plist",
        "~/Library/Preferences/com.auto2fa.app.plist",
      ]

  caveats <<~EOS
    Auto2FA installs a background helper (LaunchAgent com.auto2fa.daemon) on
    first launch. If the build isn't notarized, macOS Gatekeeper blocks the
    first launch — open it once via System Settings → Privacy & Security →
    "Open Anyway".

    To remove EVERYTHING (saved hosts + Keychain credentials), run:
      brew uninstall --zap --cask auto2fa
  EOS
end
