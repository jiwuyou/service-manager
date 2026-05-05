#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="service-manager"

usage() {
  cat <<'EOF'
Usage:
  scripts/install.sh [path/to/service-manager]

Environment:
  INSTALL_DIR   Override install directory (default: $PREFIX/bin on Termux, else $HOME/.local/bin)
  BIND          Bind address for guidance output (default: 127.0.0.1:8787)
  CONFIG_PATH   Override config path for guidance output
  INSTALL_SERVICE=1  Attempt to run "service-manager install-service" after install (best-effort)

Notes:
  - This script installs a prebuilt binary; it does not run cargo builds.
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
bind="${BIND:-127.0.0.1:8787}"

src_bin="${1:-}"
if [[ -z "${src_bin}" ]]; then
  script_dir="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  repo_root="$(CDPATH='' cd -- "${script_dir}/.." && pwd)"

  if [[ -x "${repo_root}/${BIN_NAME}" ]]; then
    src_bin="${repo_root}/${BIN_NAME}"
  elif [[ -x "${repo_root}/target/release/${BIN_NAME}" ]]; then
    src_bin="${repo_root}/target/release/${BIN_NAME}"
  fi
fi

if [[ -z "${src_bin}" || ! -f "${src_bin}" ]]; then
  echo "error: missing binary. Provide path/to/${BIN_NAME} or build it first (cargo build --release)." >&2
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
