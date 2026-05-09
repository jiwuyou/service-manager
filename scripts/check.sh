#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="service-manager"

script_dir="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH='' cd -- "${script_dir}/.." && pwd)"

bin_path=""
if command -v "${BIN_NAME}" >/dev/null 2>&1; then
  bin_path="$(command -v "${BIN_NAME}")"
elif [[ -x "${repo_root}/${BIN_NAME}" ]]; then
  bin_path="${repo_root}/${BIN_NAME}"
elif [[ -x "${repo_root}/target/release/${BIN_NAME}" ]]; then
  bin_path="${repo_root}/target/release/${BIN_NAME}"
elif [[ -x "${repo_root}/target/debug/${BIN_NAME}" ]]; then
  bin_path="${repo_root}/target/debug/${BIN_NAME}"
fi

if [[ -z "${bin_path}" ]]; then
  echo "error: missing '${BIN_NAME}' binary on PATH and in repo build outputs." >&2
  echo >&2
  echo "build (from repo root):" >&2
  echo "  cargo build --release" >&2
  echo >&2
  echo "install (prebuilt binary):" >&2
  echo "  ${repo_root}/scripts/install.sh path/to/${BIN_NAME}" >&2
  exit 1
fi

echo "ok: ${BIN_NAME} binary: ${bin_path}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"

# Compute default config locations (matches scripts/install.sh and scripts/uninstall.sh).
if [[ -n "${CONFIG_PATH:-}" ]]; then
  cfg_path="${CONFIG_PATH}"
elif [[ "${os}" == "darwin" ]]; then
  cfg_path="${HOME}/Library/Application Support/service-manager/config.json"
else
  cfg_path="${XDG_CONFIG_HOME:-${HOME}/.config}/service-manager/config.json"
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

echo
echo "token:"
echo "  ${BIN_NAME} token show"
echo "  ${BIN_NAME} token rotate"
echo
echo "auth env override:"
echo "  export SERVICE_MANAGER_TOKEN=...   # overrides config when config token is empty"

echo
echo "serve:"
echo "  ${BIN_NAME} serve --bind 127.0.0.1:8787"

url="${SERVICE_MANAGER_URL:-http://127.0.0.1:8787/}"

if command -v curl >/dev/null 2>&1; then
  if curl -fsS --max-time 2 "${url}" >/dev/null 2>&1; then
    echo
    echo "ok: service-manager responds at: ${url}"
  else
    echo
    echo "note: no response at: ${url}" >&2
    echo "  (set SERVICE_MANAGER_URL to check a different endpoint)" >&2
  fi
else
  echo
  echo "note: curl not found; skipping HTTP health check for: ${url}" >&2
fi

