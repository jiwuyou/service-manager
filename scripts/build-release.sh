#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="service-manager"

usage() {
  cat <<'EOF'
Usage:
  scripts/build-release.sh

Builds:
  - cargo build --release
  - dist/service-manager-<platform>.tar.gz

Platform:
  - Linux/macOS: service-manager-<version>-<os>-<arch>.tar.gz
  - Termux:      service-manager-<version>-termux-<arch>.tar.gz

Includes:
  - service-manager binary
  - scripts/
  - README.md
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

script_dir="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH='' cd -- "${script_dir}/.." && pwd)"

cd "${repo_root}"

version="$(grep -E '^version = "' Cargo.toml | head -n1 | sed -E 's/^version = "([^"]+)".*$/\1/')"
if [[ -z "${version}" ]]; then
  echo "error: failed to read version from Cargo.toml" >&2
  exit 1
fi

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
platform_os="${os}"
if [[ -n "${PREFIX:-}" && "${PREFIX}" == *"com.termux"* ]]; then
  platform_os="termux"
fi

echo "building (${platform_os}/${arch})..."
cargo build --release

bin_path="${repo_root}/target/release/${BIN_NAME}"
if [[ ! -x "${bin_path}" ]]; then
  echo "error: missing built binary: ${bin_path}" >&2
  exit 1
fi

dist="${repo_root}/dist"
mkdir -p "${dist}"

pkg_dir="${dist}/${BIN_NAME}-${version}-${platform_os}-${arch}"
rm -rf "${pkg_dir}"
mkdir -p "${pkg_dir}"

cp "${bin_path}" "${pkg_dir}/${BIN_NAME}"
cp -R "${repo_root}/scripts" "${pkg_dir}/scripts"
cp "${repo_root}/README.md" "${pkg_dir}/README.md"

tarball="${pkg_dir}.tar.gz"
rm -f "${tarball}"
tar -czf "${tarball}" -C "${dist}" "$(basename -- "${pkg_dir}")"

echo "wrote: ${tarball}"
