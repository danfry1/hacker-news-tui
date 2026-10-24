# Security policy

## Supported versions

Only the latest release receives security fixes. Upgrade to it before
reporting an issue, if you can.

## Reporting a vulnerability

Please report security issues privately, not in a public issue or pull request:
open this repository's **Security** tab and choose **Report a vulnerability**
(GitHub private vulnerability reporting).

Include the version (`hacker-news-tui --version`), your platform and how you installed it,
steps to reproduce, and the impact you expect. You can expect an
acknowledgement within 7 days and credit in the advisory if you want it.

In scope: the release archives on GitHub Releases, the crate on crates.io, the Homebrew formula and bottles in danfry1/homebrew-tap, and the Scoop manifest in danfry1/scoop-bucket.

## Verifying a download

Every release asset carries a signed build provenance attestation from this
repository's release workflow:

```sh
gh attestation verify hacker-news-tui-<target>.tar.gz --repo danfry1/hacker-news-tui
```

Homebrew bottles are attested by the tap's publish workflow:

```sh
gh attestation verify <bottle>.tar.gz --repo danfry1/homebrew-tap
```
