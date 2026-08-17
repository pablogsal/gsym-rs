# Releasing gsym-rs

This is the maintainer runbook for the next release. Releases are dispatched
from GitHub Actions; do not create or push the tag manually.

## Prerequisites

- The `release` environment accepts deployments from `main` and has the
  intended reviewers.
- crates.io trusted publishing names `pablogsal/gsym-rs`, `release.yml`, and
  the `release` environment.
- GitHub release immutability is enabled for the repository.
- The `Required checks` check is required on `main`.

## Prepare

1. Update the version in `Cargo.toml` to `X.Y.Z`, then refresh both workspaces:

   ```bash
   cargo update --workspace
   cargo update --manifest-path fuzz/Cargo.toml --workspace
   ```
2. Move the relevant entries from `## Unreleased` in `CHANGELOG.md` into
   `## X.Y.Z - YYYY-MM-DD`. That section becomes the GitHub release notes.
3. Run:

   ```bash
   python3 scripts/release_contract.py --tag dry-run
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
   cargo clippy --manifest-path fuzz/Cargo.toml --bins --locked -- -D warnings
   cargo test --workspace --all-features --locked
   cargo publish --package gsym-rs --dry-run --locked
   ```

4. Open and merge a pull request. Wait for every required check on `main`.

## Rehearse

Dispatch **Actions > Release > Run workflow** from `main` with `dry-run` as
the tag. The rehearsal builds and executes every Linux archive, verifies GNU
and musl linkage on native x86-64 and AArch64 runners, generates and validates
the CycloneDX SBOM, attests every archive, checks the complete payload, and
confirms trusted publishing. It does not publish the crate or create a tag or
release.

The expected targets and assets come from one release plan:

```bash
python3 scripts/release_common.py --plan X.Y.Z
```

## Publish

1. Dispatch **Actions > Release > Run workflow** from `main`.
2. Enter `vX.Y.Z` exactly and leave crate publishing enabled.
3. Approve the `release` environment when prompted.

The workflow publishes the crate only after every archive and the SBOM have
passed validation. It then revalidates the payload, signs build-provenance and
CycloneDX SBOM attestations for every archive, and creates the immutable GitHub
release and tag from the dispatched commit.

## Verify

Download the assets into an empty directory, then run:

```bash
sha256sum --check SHA256SUMS
gh release view vX.Y.Z --repo pablogsal/gsym-rs \
  --json tagName,targetCommitish,isImmutable,url,assets
gh attestation verify --repo pablogsal/gsym-rs \
  gsymtool-X.Y.Z-<target>.tar.gz
gh attestation verify --repo pablogsal/gsym-rs \
  --predicate-type https://cyclonedx.org/bom \
  gsymtool-X.Y.Z-<target>.tar.gz
cargo info gsym-rs
```

Confirm that `isImmutable` is `true`, the tag points at the intended commit on
`main`, the published asset names match the release plan, both attestations
verify, and crates.io serves `X.Y.Z`.

## Recover from failure

- Before crates.io publication: fix the problem and dispatch the same version.
- After crates.io publication but before the GitHub release: first confirm the
  crate is live, then download the run's `release-assets` artifact and create
  the release from those exact files. Do not rebuild them.
- After the immutable GitHub release exists: publish a new patch version. A
  published crate, tag, or immutable release must never be replaced.
