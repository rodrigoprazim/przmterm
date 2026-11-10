# Versioning and releases

## Scheme

PrzmTerm follows [Semantic Versioning](https://semver.org/). Every crate in the
workspace shares one version, set once in the root `Cargo.toml`
(`[workspace.package] version`).

| Change | Before 1.0 | From 1.0 |
| --- | --- | --- |
| A roadmap version (new features) | `0.Y.0` | `X.Y.0` |
| Bug fixes only | `0.Y.Z` | `X.Y.Z` |
| Breaking change to config files or CLI | `0.Y.0` | `X.0.0` |
| Release candidate | — | `1.0.0-rc.N` |

Each `0.Y.0` maps to one entry in [ROADMAP.md](../ROADMAP.md), from 0.1.0 to 1.0.0.

## Development versions

Between releases, `main` carries the next version with a `-dev` suffix, for
example `0.2.0-dev` right after 0.1.0 ships. Builds from `main` therefore
never claim to be a release.

## Changelog

Every user-visible change adds a line under `## [Unreleased]` in
[CHANGELOG.md](../CHANGELOG.md), in the same pull request as the change.

## Release procedure

1. Check the version's gate in ROADMAP.md is met and CI is green on `main`.
2. Run `scripts/release.sh 0.Y.0`. It sets the version, dates the changelog
   section, commits `release: v0.Y.0` and creates the tag `v0.Y.0`.
3. Push: `git push origin main v0.Y.0`.
4. The `release` workflow verifies the tag matches the workspace version,
   builds Linux, macOS and Windows binaries and publishes a GitHub release.
   Versions before 1.0.0 and release candidates are marked as pre-releases.
5. Bump `main` to the next `-dev` version: `scripts/release.sh --next 0.(Y+1).0`.

## Signing

The release workflow signs what it can with free services, each step only
when its secret is set, so a release still builds without them:

| What | How | Secrets and variables (repository settings) |
| --- | --- | --- |
| Windows `.exe` | Authenticode through the [SignPath Foundation](https://signpath.org) (free for open source; not approved yet, so releases are unsigned); the job waits until the request is approved on SignPath | secret `SIGNPATH_API_TOKEN`; variable `SIGNPATH_ORGANIZATION_ID`; optional `SIGNPATH_PROJECT_SLUG` (default `przmterm`) and `SIGNPATH_POLICY_SLUG` (default `release-signing`) |
| macOS app | `PrzmTerm.app` on a disk image, made by `scripts/macos-app.sh`, with an ad-hoc signature (`codesign -s -`); not notarised, which needs a paid Apple Developer account | none |
| `SHA256SUMS.txt` | detached GPG signature `SHA256SUMS.txt.asc`, covering every archive | secrets `RELEASE_GPG_KEY` (armoured private key) and `RELEASE_GPG_PASSPHRASE` |

SignPath checks the files against [.signpath/artifact-configuration.xml](../.signpath/artifact-configuration.xml),
which goes into the SignPath project: both executables must say product
`PrzmTerm` and the version being released, which `build.rs` in
`przmterm-app` and `przmterm-cli` writes into them.

The release key, made once:

```sh
gpg --quick-generate-key "PrzmTerm releases" ed25519 sign 5y
gpg --armor --export-secret-keys "PrzmTerm releases" | gh secret set RELEASE_GPG_KEY
gh secret set RELEASE_GPG_PASSPHRASE          # the passphrase chosen above
gpg --armor --export "PrzmTerm releases" > docs/release-key.asc   # commit it
```

## Test builds

*Actions › Release › Run workflow* (or `gh workflow run release.yml --ref
main`) builds the release files of a branch, named after the version and the
commit (`przmterm-1.0.0-dev+5220be3-macos-universal.dmg`), without a tag or
a release: download them from the run's *Artifacts*. Windows signing applies
as in a release; the checksums are not signed.

## Patch releases

Fixes land on `main` first. If `main` already carries features for the next
version, cherry-pick the fix to a `release/0.Y` branch cut from the `v0.Y.0`
tag and release `0.Y.1` from there.
