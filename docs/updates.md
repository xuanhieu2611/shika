# Software updates

Release builds update themselves with [Sparkle](https://sparkle-project.org) 2. Read this before changing the updater, the release scripts, the feed, or the signing keys. A mistake here can strand every installed copy on an old version, so the rules below are stricter than elsewhere.

Related: [AGENTS.md](../AGENTS.md) for the build scripts and [MANUAL_CHECKS.md](../MANUAL_CHECKS.md) for acceptance evidence.

## What the user sees

- Only the notarized DMG from `scripts/release-app.sh` contains Sparkle. `cargo run` and bundles from `scripts/bundle-app.sh` have no updater and no menu item, so a local build never replaces itself with a release.
- The notice is a small Shika card, 320px wide, using the dialog surface. It does not take focus, so typing and Escape stay in the terminal. Sparkle's own windows are not shown.
- On the second launch the card asks "Check for updates automatically?". **Check automatically** looks once a day. **Not now** leaves checks off. Either way, a download starts only from the card. Shika does not set `SUEnableAutomaticChecks` or turn on automatic download.
- With checks on, Sparkle checks once a day. A new version shows "Shika {version} is available" in the bottom-right of the terminal, with **Download** and **Ignore**. Ignore dismisses it until the next check. It does not skip the version.
- Download shows progress in that corner, then "Restart to update" in the center of the window. **Restart** quits Shika like Cmd+Q: PTYs stop and worktrees stay, and the new version opens. **Later** dismisses the card and installs the update the next time Shika quits. **Shika > Check for updates...** brings that card back. Clicks outside the centered card still reach the app.
- The menu item while nothing is pending shows "Checking for updates...". Up to date is the toast "No newer version found." A failure stays on the card until **Close**.
- Shika 0.2.0 and earlier have no updater. Their users download one more version by hand.

## How it works

- **Loading.** `updates::Updater::start` looks for `Contents/Frameworks/Sparkle.framework`, loads it with `NSBundle`, and starts `SPUUpdater` with Shika's user driver. No framework, no updater, and no "Check for updates..." item. Shika does not link Sparkle at build time, so contributors never download it. The driver answers on the main thread; the window paints `UpdateCard`.
- **Feed.** `SUFeedURL` is `https://useshika.com/appcast.xml`. `site/_redirects` sends it (302) to `https://github.com/xuanhieu2611/shika/releases/latest/download/appcast.xml`, the feed attached to the latest GitHub release. Releasing needs no site commit, and moving the feed later means changing one redirect, not shipping a build. The feed holds only the newest version.
- **Trust.** An update installs only when all of these hold:
  - The feed is signed with the EdDSA key (`SURequireSignedFeed`), so the release notes and metadata are authentic too.
  - The DMG matches its EdDSA signature before extraction (`SUVerifyUpdateBeforeExtraction`).
  - The new app is signed with the same Developer ID as the installed one.
  Neither the GitHub account nor the domain alone can push an update.
- **Versions.** Sparkle compares `sparkle:version` with the installed `CFBundleVersion`, not the marketing version. `CFBundleVersion` must go up with every release.
- **Release notes.** The release's Markdown notes are embedded in the feed (`sparkle:format="markdown"`, Sparkle 2.9 and macOS 12 or later).
- **Bundle.** `scripts/embed-sparkle.sh` copies the framework, removes its XPC services (they are for sandboxed apps, and Shika is not sandboxed), and signs Autoupdate, Updater.app, and the framework with the hardened runtime, inside out. The app is then signed without `--deep`. The DMG is APFS with lzfse (`ULFO`), as Sparkle recommends.

## Releasing

1. Bump `CFBundleShortVersionString` and `CFBundleVersion` in `assets/macos/Info.plist`, and the crate versions (then `cargo update -w`).
2. `scripts/release-app.sh` builds, embeds Sparkle, signs, notarizes, and staples `target/release/Shika.dmg`. Before building, it checks that the keychain's Sparkle key matches `SUPublicEDKey`.
3. Write the release notes as Markdown, then run `scripts/appcast.sh notes.md`. It signs the final DMG and writes and signs `target/release/appcast.xml`. It refuses to run when the build number is not above the one in the published feed.
4. Publish both files to one release, marked latest:

   ```sh
   gh release create v0.3.0 target/release/Shika.dmg target/release/appcast.xml \
       --title "Shika 0.3.0" --notes-file notes.md --latest
   ```

The update goes live with the release. GitHub's `latest/download` can keep serving the previous release's files for about a minute (40 seconds on 0.3.0), so check `curl -sSL https://useshika.com/appcast.xml` after that. Drafts and prereleases are never served there. A release without `appcast.xml` breaks the feed for everyone until one is uploaded.

To pull back a bad release, mark the previous release latest so new checks stop offering it, then ship a fix with a higher build. Sparkle does not downgrade copies that already updated.

## Signing keys

The EdDSA private key lives in the release Mac's login keychain. `generate_keys` created it on 2026-10-08, and its public half is `SUPublicEDKey`.

- **Back it up.** Run `"$(scripts/sparkle.sh)/bin/generate_keys" -x sparkle-private-key`, store the file in a password manager, and delete the local copy. Never commit it.
- **New release Mac.** Import the backup with `generate_keys -f sparkle-private-key`.
- **Keychain prompt.** The first `sign_update` or `generate_keys -p` may ask for keychain access. Choose Always Allow.
- **If the key is lost,** installed copies reject every future update. The only path is a release users download by hand, with a new key. Do not change `SUPublicEDKey` for any other reason.

The Developer ID certificate is the second key. Back up its `.p12` the same way.

## Preview the card

This paints the corner card and does not check, download, or install:

```sh
cargo run -p shika -- --data-dir /tmp/shika-update-preview --preview-update
```

Use a disposable data directory. **Download** switches to the restart card. **Ignore**, **Later**, and **Restart** only close the preview. Run the command again to see it once more.

## Testing an update locally

Never point a test build at the real feed or at normal app data. Sparkle relaunches without `--data-dir`, so test install-on-quit rather than Install and Relaunch.

1. Back up `assets/macos/Info.plist`. Set `CFBundleIdentifier` to `com.hieule.shika.updatetest`, the version to 0.2.99 build 101, and `SUFeedURL` to `http://127.0.0.1:8765/appcast.xml`. App Transport Security exempts IP addresses, and the EdDSA signatures still apply.
2. Run `scripts/release-app.sh`, then `SHIKA_DOWNLOAD_URL=http://127.0.0.1:8765/Shika.dmg scripts/appcast.sh notes.md`. Copy the DMG and the feed into a folder and serve it with `python3 -m http.server 8765 --bind 127.0.0.1`.
3. Set build 100, version 0.2.98, and add `SUEnableAutomaticChecks` and `SUAutomaticallyUpdate` as true. Run `scripts/bundle-app.sh` and `scripts/embed-sparkle.sh target/release/Shika.app "Developer ID Application"`, then sign the app with `codesign --force --options runtime --timestamp --sign "Developer ID Application"`. Copy it out of `target/`, and restore `Info.plist`.
4. Launch the copy with `open -n <copy>/Shika.app --args --data-dir <test data>`. The server log shows the feed and the DMG. Sparkle's `Autoupdate` waits for the app to quit.
5. Quit it with `osascript -e 'tell application id "com.hieule.shika.updatetest" to quit'`. Its `Info.plist` now reads build 101, and `spctl --assess` accepts it.
6. Clean up with `defaults delete com.hieule.shika.updatetest`, and remove `~/Library/Caches/com.hieule.shika.updatetest`.

In zsh, `log` is a builtin. Use `/usr/bin/log stream --predicate 'process == "Autoupdate"'`.

## Code map

| Where | What |
| --- | --- |
| `crates/shika/src/updates.rs` | `Updater::start` loads Sparkle and starts `SPUUpdater` with Shika's driver; `UpdateCard` is the corner notice; `Updater::check` is the menu action |
| `crates/shika/src/main.rs` | `CheckForUpdates`, and the Shika menu item added only when the updater started |
| `assets/macos/Info.plist` | `SUFeedURL`, `SUPublicEDKey`, `SUVerifyUpdateBeforeExtraction`, `SURequireSignedFeed` |
| `scripts/sparkle.sh` | Pinned Sparkle version and checksum; downloads to `target/sparkle/` and checks the committed license |
| `scripts/embed-sparkle.sh` | Copies, trims, and signs the framework into a bundle |
| `scripts/release-app.sh` | Key check, embedding, signing, the APFS DMG, and notarization |
| `scripts/appcast.sh` | Build number guard, the feed, and its signatures |
| `site/_redirects` | The feed redirect on useshika.com |

## Guardrails

- Do not embed Sparkle in `bundle-app.sh` bundles. `bundle-app.sh` starts from an empty bundle so a release's framework never lingers in a local build.
- Do not sign with `--deep`, and do not enable Sparkle's XPC services or sandbox Shika without following Sparkle's sandboxing guide.
- Keep `SUFeedURL` on useshika.com. Every shipped build has it baked in; move hosting by changing `site/_redirects`.
- Keep both signature checks on. Do not weaken them to make a release go out.
- To upgrade Sparkle, change the version and checksum in `scripts/sparkle.sh`, replace `assets/licenses/Sparkle-MIT.txt` with its `LICENSE`, read its changelog, and repeat the local update test.
