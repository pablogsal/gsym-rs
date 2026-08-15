#!/usr/bin/env bash
set -euo pipefail

version=0.5.9
case "$(uname -m)" in
  x86_64)
    target=x86_64-unknown-linux-gnu
    checksum=fb8dbee9f182173e062a64a387b21a0badc6fab8b2abf9294973f012972bf6d8
    ;;
  aarch64)
    target=aarch64-unknown-linux-gnu
    checksum=7bf131ca5389b07a4f10c182bcf8a5ad339d64408b6f0d8f6834a0bd6120a06a
    ;;
  *)
    echo "cargo-cyclonedx has no reviewed release binary for $(uname -m)" >&2
    exit 1
    ;;
esac

archive="cargo-cyclonedx-${target}.tar.xz"
download_root=$(mktemp -d "${RUNNER_TEMP:-/tmp}/cargo-cyclonedx.XXXXXX")
url="https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/download/cargo-cyclonedx-${version}/${archive}"

curl --proto '=https' --tlsv1.2 --fail --location --retry 3 \
  --output "$download_root/$archive" "$url"
(
  cd "$download_root"
  printf '%s  %s\n' "$checksum" "$archive" | sha256sum --check --strict
  tar -xJf "$archive"
)

cargo_root=${CARGO_HOME:-$HOME/.cargo}
install -D -m 0755 \
  "$download_root/cargo-cyclonedx-${target}/cargo-cyclonedx" \
  "$cargo_root/bin/cargo-cyclonedx"
"$cargo_root/bin/cargo-cyclonedx" cyclonedx --version
