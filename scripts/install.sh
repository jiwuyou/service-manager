#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="service-manager"

usage() {
  cat <<'EOF'
Usage:
  scripts/install.sh [path/to/service-manager]

Environment:
  INSTALL_DIR   Override install directory (default: $PREFIX/bin on Termux, else $HOME/.local/bin)
  BIND          Bind address for guidance output (default: 127.0.0.1:20087)
  CONFIG_PATH   Override config path for guidance output
  INSTALL_SERVICE=1  Attempt to run "service-manager install-service" after install (best-effort)
  SERVICE_MANAGER_INSTALL_MODE
                    auto (default), release, local, or source.
  SERVICE_MANAGER_RELEASE_BASE_URL
                    Release asset base URL (default: GitHub latest download URL).

Notes:
  - This script prefers a supplied/local binary, then a release tarball.
  - Termux downloads use service-manager-<version>-termux-<arch>.tar.gz.
  - Source builds require SERVICE_MANAGER_INSTALL_MODE=source.
  - The server auto-creates config/data dirs and generates a token on first run.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

is_termux=0
if [[ -n "${PREFIX:-}" && "${PREFIX}" == *"com.termux"* ]]; then
  is_termux=1
fi

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
bind="${BIND:-127.0.0.1:20087}"
mode="${SERVICE_MANAGER_INSTALL_MODE:-auto}"
platform_os="${os}"
if [[ "${is_termux}" == "1" ]]; then
  platform_os="termux"
fi

script_dir="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH='' cd -- "${script_dir}/.." && pwd)"

detect_local_binary() {
  if [[ -x "${repo_root}/${BIN_NAME}" ]]; then
    printf '%s\n' "${repo_root}/${BIN_NAME}"
  elif [[ -x "${repo_root}/target/release/${BIN_NAME}" ]]; then
    printf '%s\n' "${repo_root}/target/release/${BIN_NAME}"
  else
    return 1
  fi
}

build_from_source() {
  command -v cargo >/dev/null 2>&1 || {
    echo "error: cargo not found; source install requires Rust/Cargo." >&2
    return 1
  }
  echo "building ${BIN_NAME} from source (cargo build --release)" >&2
  (cd "${repo_root}" && cargo build --release)
  [[ -x "${repo_root}/target/release/${BIN_NAME}" ]] || {
    echo "error: missing built binary: ${repo_root}/target/release/${BIN_NAME}" >&2
    return 1
  }
  printf '%s\n' "${repo_root}/target/release/${BIN_NAME}"
}

download_release_binary() {
  command -v curl >/dev/null 2>&1 || {
    echo "error: curl not found; cannot download release binary." >&2
    return 1
  }
  command -v tar >/dev/null 2>&1 || {
    echo "error: tar not found; cannot unpack release binary." >&2
    return 1
  }

  version="$(grep -E '^version = "' "${repo_root}/Cargo.toml" | head -n1 | sed -E 's/^version = "([^"]+)".*$/\1/')"
  [[ -n "${version}" ]] || {
    echo "error: failed to read version from Cargo.toml" >&2
    return 1
  }

  base_url="${SERVICE_MANAGER_RELEASE_BASE_URL:-https://github.com/jiwuyou/service-manager/releases/latest/download}"
  asset="${BIN_NAME}-${version}-${platform_os}-${arch}.tar.gz"
  url="${base_url%/}/${asset}"
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/service-manager-install.XXXXXX")"
  cleanup_download() {
    rm -rf "${tmp_dir}" >/dev/null 2>&1 || true
  }
  trap cleanup_download RETURN

  echo "downloading release: ${url}" >&2
  curl -fL --connect-timeout 20 --retry 3 --retry-delay 2 --retry-all-errors "${url}" -o "${tmp_dir}/${asset}"
  tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"

  found="$(find "${tmp_dir}" -type f -name "${BIN_NAME}" -perm -111 | head -n1 || true)"
  [[ -n "${found}" ]] || {
    echo "error: release archive did not contain an executable ${BIN_NAME}" >&2
    return 1
  }
  mkdir -p "${repo_root}/target/release"
  cp "${found}" "${repo_root}/target/release/${BIN_NAME}"
  chmod 0755 "${repo_root}/target/release/${BIN_NAME}"
  printf '%s\n' "${repo_root}/target/release/${BIN_NAME}"
}

src_bin="${1:-}"
if [[ -z "${src_bin}" ]]; then
  case "${mode}" in
    auto)
      src_bin="$(detect_local_binary || download_release_binary)"
      ;;
    local)
      src_bin="$(detect_local_binary)"
      ;;
    release)
      src_bin="$(download_release_binary)"
      ;;
    source)
      src_bin="$(build_from_source)"
      ;;
    *)
      echo "error: unknown SERVICE_MANAGER_INSTALL_MODE: ${mode}" >&2
      exit 1
      ;;
  esac
fi

if [[ -z "${src_bin}" || ! -f "${src_bin}" ]]; then
  echo "error: missing binary. Provide path/to/${BIN_NAME}, use SERVICE_MANAGER_INSTALL_MODE=release, or use SERVICE_MANAGER_INSTALL_MODE=source." >&2
  exit 1
fi

if [[ ! -x "${src_bin}" ]]; then
  echo "error: binary is not executable: ${src_bin}" >&2
  exit 1
fi

install_dir="${INSTALL_DIR:-}"
if [[ -z "${install_dir}" ]]; then
  if [[ "${is_termux}" == "1" ]]; then
    install_dir="${PREFIX}/bin"
  else
    install_dir="${HOME}/.local/bin"
  fi
fi

mkdir -p "${install_dir}"

dst_bin="${install_dir}/${BIN_NAME}"

if command -v install >/dev/null 2>&1; then
  install -m 0755 "${src_bin}" "${dst_bin}"
else
  cp "${src_bin}" "${dst_bin}"
  chmod 0755 "${dst_bin}"
fi

echo "installed: ${dst_bin}"

case ":${PATH}:" in
  *":${install_dir}:"*)
    ;;
  *)
    echo "note: ${install_dir} is not on PATH"
    echo "  export PATH=\"${install_dir}:\$PATH\""
    ;;
esac

# Compute default config locations (matches src/server.rs).
cfg_dir=""
if [[ -n "${CONFIG_PATH:-}" ]]; then
  cfg_path="${CONFIG_PATH}"
elif [[ "${os}" == "darwin" ]]; then
  cfg_dir="${HOME}/Library/Application Support"
  cfg_path="${cfg_dir}/service-manager/config.json"
else
  cfg_dir="${XDG_CONFIG_HOME:-${HOME}/.config}"
  cfg_path="${cfg_dir}/service-manager/config.json"
fi

data_dir=""
if [[ "${cfg_path}" == */service-manager/config.json ]]; then
  data_dir="${cfg_path%/config.json}/data"
fi

echo
echo "config: ${cfg_path}"
if [[ -n "${data_dir}" ]]; then
  echo "data:   ${data_dir}"
fi

mkdir -p "$(dirname -- "${cfg_path}")"
if [[ -n "${data_dir}" ]]; then
  mkdir -p "${data_dir}"
fi

echo
echo "next:"
echo "  ${BIN_NAME} serve --bind ${bind}"
echo
echo "web:"
echo "  http://${bind}/"
echo
echo "token:"
echo "  ${BIN_NAME} token show"
echo "  ${BIN_NAME} token rotate"
echo
echo "auth env override:"
echo "  export SERVICE_MANAGER_TOKEN=...   # overrides config when config token is empty"

if [[ "${INSTALL_SERVICE:-0}" == "1" ]]; then
  echo
  echo "install-service: attempting (best-effort)"
  "${dst_bin}" install-service || true
fi
