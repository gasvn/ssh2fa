# Releasing SSH2FA

This is the end-to-end recipe to ship a signed, notarized `SSH2FA.dmg` that
runs on any Mac (not just the build machine).

## TL;DR

```sh
# one-time: enroll in the Apple Developer Program, create a Developer ID cert,
# and store notarization credentials (see "Apple account setup" below).

cd auto2fa-mac
AUTO2FA_NOTARIZE=1 AUTO2FA_NOTARY_PROFILE=ssh2fa-notary ./package-app.sh
# → dist/SSH2FA.dmg   (signed, notarized, stapled, universal)
```

Without an Apple Developer ID the script still builds a working **local**
`.app`/DMG (signed with your Apple Development cert) — it just skips
notarization and prints exactly why. That artifact runs on *your* machine but
Gatekeeper will block it on others.

## What `package-app.sh` does

1. Builds the **universal** (`arm64` + `x86_64`) Rust daemon via `lipo`
   (falls back to arm64-only if the x86_64 target isn't installed).
2. `xcodebuild`s the Release app.
3. Copies `ssh2fa-daemon` into `SSH2FA.app/Contents/Resources/` (the app's
   first-run installer copies it to `~/.ssh2fa/` and registers the
   LaunchAgent).
4. Signs the embedded daemon (pinned identifier `com.ssh2fa.daemon`) then the
   `.app`, with **hardened runtime** + the entitlements in
   `auto2fa-mac/SSH2FA.entitlements`. Prefers a **Developer ID Application**
   cert; falls back to **Apple Development** (local only).
5. Builds `dist/SSH2FA.dmg`.
6. If `AUTO2FA_NOTARIZE=1` **and** a Developer ID cert is present: submits the
   DMG to Apple's notary service, waits, and **staples** the ticket to both the
   app and the DMG.

## Apple account setup (one time)

The notarization step is the only thing that requires *your* Apple credentials —
it cannot be done without them.

1. **Enroll** in the Apple Developer Program ($99/yr):
   <https://developer.apple.com/programs/>.
2. In Xcode → Settings → Accounts, add your Apple ID and **Manage
   Certificates → + → Developer ID Application**. Confirm it's installed:
   ```sh
   security find-identity -v -p codesigning | grep "Developer ID Application"
   ```
3. Create an **app-specific password** at <https://appleid.apple.com> (Sign-In
   & Security → App-Specific Passwords) and store notarization credentials in a
   keychain profile:
   ```sh
   xcrun notarytool store-credentials ssh2fa-notary \
     --apple-id "you@example.com" \
     --team-id   "YOURTEAMID" \
     --password  "abcd-efgh-ijkl-mnop"   # the app-specific password
   ```
   (`YOURTEAMID` is the 10-char Team ID shown in the certificate / your
   developer account.)

Then run the TL;DR command. Subsequent releases are just that one command.

## Universal binary

The app is universal by default (`xcodebuild` builds both arches). For a
universal **daemon**:

```sh
rustup target add x86_64-apple-darwin   # one time
```

`package-app.sh` (and `auto2fa-rs/build-release.sh`) detect the installed target
and `lipo` the two slices together automatically. If you only ship Apple
Silicon, skip this — the script produces an arm64-only daemon and says so.

## Cutting a GitHub release

The app's **Check for Updates** (Settings → About) compares this build's
version to the latest GitHub release tag. To make it work:

1. Bump the version in `auto2fa-mac/SSH2FA/Resources/Info.plist`
   (`CFBundleShortVersionString` + `CFBundleVersion`) and
   `MARKETING_VERSION` in the xcodeproj.
2. `git tag vX.Y.Z && git push --tags`.
3. Create a GitHub Release for that tag and attach `dist/SSH2FA.dmg`.

Tags may be `vX.Y.Z` or `X.Y.Z`; the checker strips a leading `v`.

## Project landing page (GitHub Pages)

A one-page site lives at [`docs/index.html`](index.html) and is **live at
<https://shgao.site/ssh2fa/>** (GitHub Pages, deploy-from-branch `main` → `/docs`).

Note: `gasvn` has a user-level custom domain (`shgao.site`), so project pages
resolve at `shgao.site/<repo>/` rather than `gasvn.github.io/<repo>/`. To
(re)configure: Repo **Settings → Pages → Source: Deploy from a branch → `main`
→ `/docs`**.

`docs/.nojekyll` makes GitHub serve the HTML as-is. The
markdown docs (README/SECURITY/RELEASE) stay GitHub-rendered; the landing page
links to them.

## Homebrew cask

The cask lives at [`Casks/ssh2fa.rb`](../Casks/ssh2fa.rb) and is published through
the tap repo **[`gasvn/homebrew-tap`](https://github.com/gasvn/homebrew-tap)**
(Homebrew requires a tap repo named `homebrew-*`; `homebrew-tap` → the `gasvn/tap`
shorthand). The tap is general — it can hold any future casks/formulae too.

Users install with:

```sh
# install + clear Gatekeeper + open (works on every Homebrew version):
brew install --cask gasvn/tap/ssh2fa \
  && xattr -dr com.apple.quarantine /Applications/SSH2FA.app \
  && open /Applications/SSH2FA.app
brew upgrade --cask ssh2fa            # update to the latest release
brew uninstall --zap --cask ssh2fa    # full removal incl. Keychain creds
```

Because the app is un-notarized, Homebrew quarantines it on install, so the first
launch is blocked until the quarantine flag is cleared. **Do not use the old
`--no-quarantine` install flag — Homebrew removed it (newer versions error with
`invalid option: --no-quarantine`).** Instead clear it after install with
`xattr -dr com.apple.quarantine /Applications/SSH2FA.app` (as above), or allow it
once via System Settings → Privacy & Security → "Open Anyway". Brew does **not**
notarize; only a Developer ID + the notary service does (see above).

**Each release — keep the cask in sync in BOTH repos:**

1. After `package-app.sh`, copy the printed **DMG sha256**.
2. Bump `version` + `sha256` in **`Casks/ssh2fa.rb`** (this repo) **and** in
   `gasvn/homebrew-tap`'s `Casks/ssh2fa.rb`, then push the tap. (Both must match
   the uploaded DMG or `brew install` fails the checksum.)

The cask quits the app + unloads the LaunchAgent on uninstall; `--zap` also
trashes `~/.ssh2fa`, the LaunchAgent plist, prefs, and every Keychain credential
under the `auto2fa` service. Validate edits with `brew style ./Casks/ssh2fa.rb`
and `brew audit --cask gasvn/tap/ssh2fa`.

## Future: Sparkle auto-update

The current updater only *notifies* (it never downloads/installs — the user
stays in control of what runs, since the app holds SSH creds). If you later
want true auto-update, integrate [Sparkle](https://sparkle-project.org): add the
SPM package, generate an EdDSA key pair, host an `appcast.xml`, and sign each
DMG with the Sparkle key. That's a deliberate, separate step — not required for
a first release.

## Notes carried from hard-won experience

- **Never** set `panic = "abort"` in any `Cargo.toml` — the daemon's stability
  model relies on `catch_unwind` + unwinding.
- The daemon is signed with a **pinned identifier** so the Keychain grant
  survives rebuilds. The Apple Development cert **auto-rotates yearly**; the
  first launch after a rotation can stall in `xpcproxy`/`amfid` for 1–3 minutes
  while macOS re-validates — don't panic-kill it.
- Deploying the daemon by hand to a dev machine: build → codesign → `mv` to
  `~/.ssh2fa/ssh2fa-daemon` → `kill -9` the running one (launchd respawns and
  re-adopts live masters → zero relogin). The packaged app does this install
  itself on first run.
- **The LaunchAgent runs the daemon IN PLACE from inside the app bundle**
  (`SSH2FA.app/Contents/Resources/ssh2fa-daemon`), it is NOT copied to
  `~/.ssh2fa`. This is deliberate: a daemon signed with an *Apple Development*
  cert (the free, un-notarized build) that is **copied** to a new path is
  refused at exec by the kernel (`launchctl print …` →
  `last exit reason = OS_REASON_EXEC`) even though `codesign -v` passes — an
  AMFI per-path quirk. Running it where it was signed sidesteps that, and app
  updates update the daemon automatically. The first-run installer re-points
  the LaunchAgent on every launch, so moving the app (e.g. into /Applications)
  self-heals. On the clean-machine test, confirm
  `launchctl print gui/$UID/com.ssh2fa.daemon` shows `state = running` after
  first launch. (If you hand-deploy a daemon to `~/.ssh2fa` on a dev machine
  instead, re-sign it **in place** after any copy — never run a copied
  Apple-Development-signed binary.)
- **Don't exec the deployed daemon from the dev shell to test it** — a binary
  under `$HOME` exec'd from the (sandboxed) dev shell is SIGKILLed (exit 137),
  a false negative. Test via launchd (`launchctl kickstart`/`print`).
