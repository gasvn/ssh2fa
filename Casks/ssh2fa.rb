cask "ssh2fa" do
  version :latest
  # The DMG isn't reproducible (its checksum changes on every build), so we track
  # the latest release asset directly and skip the checksum — transport security
  # is GitHub's HTTPS, and the app is un-notarized regardless. This also means the
  # cask never needs a per-release sha bump. Update an existing install with:
  #   brew reinstall --cask ssh2fa     (or: brew upgrade --cask --greedy)
  sha256 :no_check

  url "https://github.com/gasvn/ssh2fa/releases/latest/download/SSH2FA.dmg"
  name "SSH2FA"
  desc "Keeps SSH ControlMaster pools warm and auto-answers Duo/TOTP 2FA logins"
  homepage "https://github.com/gasvn/ssh2fa"

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
