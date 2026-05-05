# service-manager

Local-only service manager (CLI + REST API) with a static Web UI.

The server binds to `127.0.0.1:8787` by default and protects all `/api/v1/*` endpoints with a bearer
token (except `/api/v1/health`).

## Install

### Prebuilt (recommended)

1. Download a release tarball and unpack it.
2. Run:

```bash
./scripts/install.sh ./service-manager
```

This installs the binary into:

- Termux: `$PREFIX/bin`
- Other Unix: `$HOME/.local/bin`

Override with `INSTALL_DIR=/some/bin ./scripts/install.sh ./service-manager`.

### From source

```bash
cargo build --release
./scripts/install.sh ./target/release/service-manager
```

### Termux + proot notes

This repo is commonly developed inside a proot Linux environment at `/root/service-manager`.

- If you want to run the binary inside proot, build and install inside proot.
- If you want to run the binary in outer Termux (`$PREFIX/bin`), build it with the Termux toolchain
  (binaries built against glibc in a proot distro may not run in plain Termux).

## Uninstall

Preserve config/data (default):

```bash
./scripts/uninstall.sh
```

Purge config/data (explicit, guarded):

```bash
./scripts/uninstall.sh --purge
```

Safety guard: `--purge` only deletes directories whose basename is exactly `service-manager` (for
example `$HOME/.config/service-manager`). If a safe directory cannot be derived, the script refuses
to purge.

## Run

Start the server:

```bash
service-manager serve
```

Override bind/config paths:

```bash
service-manager serve --bind 127.0.0.1:8787 --config /path/to/config.json
```

Web UI (served at the same origin as the API):

- `http://127.0.0.1:8787/`

Token utilities:

```bash
service-manager token show
service-manager token rotate
```

Environment override:

- `SERVICE_MANAGER_TOKEN` is only used when `auth_token` in the config is empty.

Diagnostics:

```bash
service-manager doctor
```

## Config + Data

Config is JSON. Defaults (when `--config` is not provided):

- Linux: `${XDG_CONFIG_HOME:-$HOME/.config}/service-manager/config.json`
- macOS: `$HOME/Library/Application Support/service-manager/config.json`

Default data dir:

- `${UserConfigDir}/service-manager/data/`

Default store:

- `${data_dir}/store.json` (atomic JSON store)

Example config:

```json
{
  "listen_addr": "127.0.0.1:8787",
  "data_dir": "/home/me/.config/service-manager/data",
  "auth_token": "",
  "log_level": "info",
  "store": { "type": "json", "path": "" }
}
```

Notes:

- If `auth_token` is empty on first run, a token is generated and persisted to the config file.
- If `store.path` is empty, it defaults to `${data_dir}/store.json`.

## REST API (v1)

Health (no auth):

- `GET /api/v1/health`

All other endpoints require:

- `Authorization: Bearer <token>`

Endpoints:

- `GET /api/v1/providers`
- `GET /api/v1/services`
- `POST /api/v1/services` (body: `ServiceSpec`)
- `GET /api/v1/services/:id`
- `PUT /api/v1/services/:id` (body: `ServiceSpec`)
- `DELETE /api/v1/services/:id`
- `POST /api/v1/services/:id/register`
- `POST /api/v1/services/:id/unregister`
- `POST /api/v1/services/:id/start`
- `POST /api/v1/services/:id/stop`
- `POST /api/v1/services/:id/restart`
- `GET /api/v1/services/:id/status`
- `GET /api/v1/services/:id/logs?since=&until=&limit=`
- `GET /api/v1/audit?limit=`
- `GET /api/v1/export`
- `POST /api/v1/import` (body: JSON export)

Error shape:

```json
{ "error": { "code": "bad_request", "message": "..." } }
```

## Providers

`GET /api/v1/providers` returns per-provider diagnostics:

- `id`, `display_name`, `description`
- `capabilities` (`register`, `start`, `stop`, `restart`, `status`, `logs`, ...)
- `detected` plus `detect_error` / `detect_details`

The Web UI uses `capabilities` to enable/disable actions.

## Web UI (static)

Static UI files live in `web/` and are intended to be embedded/served by Rust via
`src/assets.rs` (no Node build).

- The UI stores the bearer token in browser `localStorage`.
- API calls are same-origin to `/api/v1/*`.

## Troubleshooting

Unauthorized in UI/curl:

- Confirm the token: `service-manager token show`
- Ensure you’re sending `Authorization: Bearer <token>`

Bind failures:

- Another process is using the port. Try `--bind 127.0.0.1:0` for an ephemeral port or pick a new
  port.

Provider not detected:

- Check `GET /api/v1/providers` for `detect_error` / `detect_details`.
