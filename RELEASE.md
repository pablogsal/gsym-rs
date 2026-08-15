# Releasing gsym-rs

Releases are dispatched from GitHub Actions, never by pushing a tag. The
workflow builds and verifies every artifact first, and only then creates the
tag, the GitHub release, and the crates.io publication. Tags have the exact
form `vX.Y.Z` and always match `package.version` in `Cargo.toml`.

## One-time repository setup

1. Create a repository environment named `release` (Settings → Environments).
   Every job that can publish something runs in it, so protection rules and
   required reviewers apply to all of them. Restrict its deployment branches to
   `main`.
2. Configure crates.io trusted publishing for the `gsym-rs` crate
   (crates.io → the crate → Settings → Trusted Publishing) with:

   | Field       | Value                |
   | ----------- | -------------------- |
   | Repository  | `pablogsal/gsym-rs`  |
   | Workflow    | `release.yml`        |
   | Environment | `release`            |

   crates.io can only trust a crate that already exists, so the very first
   `0.1.0` publication has to be made once from a workstation with
   `cargo publish --package gsym-rs --locked`. Every later release then goes
   through trusted publishing and no registry token is ever stored in GitHub.
   For that first release, wait until crates.io serves the manual publication,
   then dispatch the workflow with **Publish the library to crates.io**
   unchecked. The workflow requires the version to exist before it creates the
   tag and GitHub release.
3. Protect `main` and require the `CI complete` status check.
4. Every release archive gets a build provenance attestation. If the repository
   cannot produce them, delete the attestation step from
   `.github/workflows/release.yml`.

## Prepare the release

1. Set `package.version` in `Cargo.toml` and `gsymtool/Cargo.toml` to `X.Y.Z`
   and refresh `Cargo.lock` (`cargo update --workspace`).
2. Move the notes for this release out of `## Unreleased` into a new section
   titled exactly `## X.Y.Z - YYYY-MM-DD`. That section becomes the GitHub
   release body.
3. Run the local checks. The last two build and verify a real archive, exactly
   as the workflow does:

   ```bash
   python3 scripts/release_contract.py --tag dry-run
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
   cargo test --workspace --all-features --locked
   cargo publish --package gsym-rs --dry-run --locked

   host=$(rustc -vV | sed -n 's/^host: //p')
   cargo build --package gsymtool --release --locked --target "$host"
   python3 scripts/build_release_archive.py \
     --binary "target/$host/release/gsymtool" --target "$host" --version X.Y.Z
   python3 scripts/verify_release_archive.py --target "$host" --version X.Y.Z
   ```

4. Open a pull request. The `Release` workflow also runs on pull requests that
   touch release inputs and re-checks the contracts there.

## Rehearse

Dispatch **Release** from `main` with the default `dry-run` tag. It builds,
archives, and verifies every target, and checks crates.io trusted publishing,
without creating a tag, a release, or a publication. The trusted publishing
check enters the `release` environment, so a rehearsal waits for the same
approval a real release does, and dispatching from another branch is refused by
the environment's deployment branch rule.

Pushing the candidate commit to a branch named `release-ci-test/<something>`
rehearses the artifacts without that approval, and therefore without the
crates.io check.

`scripts/release_common.py` is the single source of truth for the platforms:
the build matrix, the archive names, and the published asset set are all
generated from it, and the workflow fails if what it built differs. To see what
a given version must produce:

```bash
python3 scripts/release_common.py --plan X.Y.Z
```

Every target is built and verified on a native runner of its own architecture.

## Publish

1. Merge the release commit into `main` and let CI pass.
2. Open **Actions → Release → Run workflow**, select `main`, and enter the
   exact tag `vX.Y.Z`.
3. Approve the `release` environment when prompted.

The workflow then, in order:

1. validates the source, changelog, lockfile, and tag contracts, and confirms
   that the tag, the GitHub release, and the crates.io version are all free;
2. mints a crates.io trusted publishing token and assembles the package, so a
   misconfigured publisher fails before anything is created;
3. builds `gsymtool` for every target on native runners, checks the glibc
   baseline of the GNU builds and the static linkage of the musl builds, runs
   the packaged binary, and checksums each archive;
4. assembles the exact release payload and re-validates every checksum;
5. publishes the library to crates.io and waits until the registry serves it;
6. re-runs the contracts, attests build provenance, and creates the tag and the
   GitHub release from the dispatched commit.

## Verify

```bash
gh release view vX.Y.Z --repo pablogsal/gsym-rs --json tagName,targetCommitish,url,assets
gh attestation verify --repo pablogsal/gsym-rs gsymtool-X.Y.Z-<target>.tar.gz
sha256sum --check SHA256SUMS
cargo info gsym-rs
```

The workflow already refuses to finish unless the published asset set matches
the plan and the tag points at the dispatched commit; this is the independent
look. Confirm that the tag is the commit you intended on `main` and that
crates.io serves the new version.

## If something fails

A failure at or before step 4 leaves nothing behind: fix the cause and dispatch
the same tag again.

A failure in step 5 also leaves no tag and no release, so the same tag can be
dispatched again once the cause is fixed, unless the crate reached crates.io
and only the availability poll failed. Check `cargo info gsym-rs` first; if the
version is live, finish the release by hand instead (below).

A failure in step 6 leaves the crate published but the tag and release missing.
Nothing needs rebuilding: download the `release-assets` artifact from the run,
which is the exact verified payload, and publish it yourself.

```bash
gh run download <run-id> --name release-assets --dir release-assets
python3 scripts/release_contract.py --tag vX.Y.Z --notes-out release-notes.md
gh release create vX.Y.Z --target <commit> --title "gsym-rs X.Y.Z" \
  --notes-file release-notes.md release-assets/*
```

Published crates.io versions, tags, and releases cannot be replaced. Use a new
patch version for any correction.
