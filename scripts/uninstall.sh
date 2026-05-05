#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="service-manager"

usage() {
  cat <<'EOF'
Usage:
  scripts/uninstall.sh [--purge] [--service] [--bin /path/to/service-manager]

Options:
  --bin PATH   Uninstall the binary at PATH (default: uses `command -v service-manager`)
  --purge      Also delete config/data directory (default: preserve data)
  --service    Attempt to run "service-manager uninstall-service" before removing the binary (best-effort)
  --help       Show help

Environment:
  CONFIG_PATH  Override config path for purge location calculations
EOF
}

purge=0
service=0
bin_path=""

die() {
  echo "error: $*" >&2
  exit 1
}

is_safe_purge_dir() {
  # Purge only known service-manager-owned directories:
  # - any directory whose basename is exactly "service-manager"
  # This prevents accidental `rm -rf $HOME/.config` when CONFIG_PATH is a file like
  # "$HOME/.config/service-manager.json".
  local d="${1:-}"
  [[ -n "${d}" ]] || return 1
  [[ "${d}" != "/" ]] || return 1
  [[ "$(basename -- "${d}")" == "service-manager" ]] || return 1
  return 0
}

add_purge_target() {
  local d="${1:-}"
  [[ -n "${d}" ]] || return 0
  if ! is_safe_purge_dir "${d}"; then
    return 0
  fi
  purge_targets="${purge_targets}${d}
"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --purge)
      purge=1
      shift
      ;;
    --service)
      service=1
      shift
      ;;
    --bin)
      bin_path="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown arg: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${bin_path}" ]]; then
  if command -v "${BIN_NAME}" >/dev/null 2>&1; then
    bin_path="$(command -v "${BIN_NAME}")"
  fi
fi

if [[ "${service}" == "1" && -n "${bin_path}" && -x "${bin_path}" ]]; then
  echo "uninstall-service: attempting (best-effort)"
  "${bin_path}" uninstall-service || true
fi

if [[ -n "${bin_path}" && -f "${bin_path}" ]]; then
  rm -f "${bin_path}"
  echo "removed: ${bin_path}"
else
  echo "note: binary not found; nothing removed" >&2
fi

os="$(uname -s | tr '[:upper:]' '[:lower:]')"

# Compute default config locations (matches src/server.rs).
if [[ -n "${CONFIG_PATH:-}" ]]; then
  cfg_path="${CONFIG_PATH}"
elif [[ "${os}" == "darwin" ]]; then
  cfg_path="${HOME}/Library/Application Support/service-manager/config.json"
else
  cfg_path="${XDG_CONFIG_HOME:-${HOME}/.config}/service-manager/config.json"
fi

purge_targets=""

# If CONFIG_PATH is the default ".../service-manager/config.json", purge the directory.
if [[ "${cfg_path}" == */service-manager/config.json ]]; then
  add_purge_target "$(dirname -- "${cfg_path}")"
fi

# If CONFIG_PATH is explicitly pointing at a directory, only accept it if it is ".../service-manager".
if [[ -d "${cfg_path}" ]]; then
  add_purge_target "${cfg_path}"
fi

# Termux/system-style locations.
is_termux=0
if [[ -n "${PREFIX:-}" && "${PREFIX}" == *"com.termux"* ]]; then
  is_termux=1
fi

if [[ "${is_termux}" == "1" ]]; then
  add_purge_target "${PREFIX}/etc/service-manager"
  add_purge_target "${PREFIX}/var/lib/service-manager"
fi

if [[ "${purge}" == "1" ]]; then
  if [[ -z "${purge_targets}" ]]; then
    echo "refusing to purge: no safe service-manager directory could be derived." >&2
    echo "CONFIG_PATH was: ${cfg_path}" >&2
    echo "Allowed purge paths must end with '/service-manager' (basename exactly 'service-manager')." >&2
    exit 2
  fi
  printf "%s" "${purge_targets}" | while IFS= read -r d; do
    [[ -n "${d}" ]] || continue
    if ! is_safe_purge_dir "${d}"; then
      die "refusing to purge unsafe path: ${d}"
    fi
    rm -rf "${d}"
    echo "purged: ${d}"
  done
else
  # For display, prefer the derived config directory path if it looks like the standard layout.
  if [[ "${cfg_path}" == */service-manager/config.json ]]; then
    echo "preserved: $(dirname -- "${cfg_path}")"
  else
    echo "preserved: (no purge requested)"
    echo "  (CONFIG_PATH was: ${cfg_path})"
  fi
  echo "  (use --purge to delete config/data)"
fi
