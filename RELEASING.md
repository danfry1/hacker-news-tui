# Releasing hacker-news-tui

## Before tagging

1. Bump `version` in `Cargo.toml` and run `cargo check` so `Cargo.lock` follows.
2. Add the release's section to `CHANGELOG.md`.
3. Merge that to `main` through a pull request (main requires CI to pass).

## Tag

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

The tag runs `.github/workflows/release.yml`:

- `create-release` and `upload` build every target and attach the archives and
  their `.sha256` files to a GitHub Release.
- `publish-crate` publishes to crates.io through Trusted Publishing (OIDC; no
  stored token).
- `scoop-bump` updates `danfry1/scoop-bucket` with the GitHub App token.
- `homebrew-formula` opens the Homebrew tap pull request (below).
- `attest` signs build provenance for the release assets.

## Publish the Homebrew bottles

The release's `homebrew-formula` job opens a pull request on
[danfry1/homebrew-tap](https://github.com/danfry1/homebrew-tap) (branch
`hacker-news-tui-<version>`) that points `hacker-news-tui.rb` at the tag's source archive. The tap's CI
(`brew test-bot`) builds and tests bottles on it for Apple Silicon macOS 15+,
x86_64 Linux and ARM Linux, in a few minutes.

1. Wait for that pull request's checks to pass.
2. Run the publish command the bot posts as a comment on it:

   ```sh
   gh workflow run publish.yml -R danfry1/homebrew-tap -f pull_request=<N> -f head_sha=<sha>
   ```

   `brew pr-pull` uploads the bottles to a release in the tap, attests them,
   commits the bottle block and closes the pull request.

**Never merge the bot's pull request with the merge button.** That ships a
formula without bottles, so every install compiles from source until the next
publish.

## Check the release

- The workflow run is green, including `attest` (signed build provenance for
  every release asset): `gh attestation verify <file> --repo danfry1/hacker-news-tui`.
- `brew update && brew upgrade hacker-news-tui` pours a bottle (`brew info hacker-news-tui` shows the
  new version).
