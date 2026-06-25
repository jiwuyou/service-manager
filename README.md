# service-manager

Local-only service manager (CLI + REST API) with a static Web UI.

Documentation:

- Chinese usage guide: [docs/usage.zh-CN.md](docs/usage.zh-CN.md)

The server binds to `127.0.0.1:20087` by default and protects all `/api/v1/*` endpoints with a bearer
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
service-manager serve --bind 127.0.0.1:20087 --config /path/to/config.json
```

Web UI (served at the same origin as the API):

- `http://127.0.0.1:20087/`

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

OpenHouseAI service registry:

- `${XDG_CONFIG_HOME:-$HOME/.config}/openhouseai/service-manager/services.d/*.json`
- Override with `service_registry_dir` in config JSON.
- Registry files are loaded on `service-manager serve` startup and upsert services by stable id.

Example config:

```json
{
  "listen_addr": "127.0.0.1:20087",
  "data_dir": "/home/me/.config/service-manager/data",
  "service_registry_dir": "/home/me/.config/openhouseai/service-manager/services.d",
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
- `GET /api/v1/groups`
- `GET /api/v1/groups/:name/status`
- `POST /api/v1/groups/:name/start`
- `POST /api/v1/groups/:name/stop`
- `POST /api/v1/groups/:name/restart`
- `GET /api/v1/services`
- `GET /api/v1/services?tag=<tag>&group=<group>`
- `GET /api/v1/services/statuses?tag=<tag>&group=<group>`
- `POST /api/v1/services` (body: `ServiceSpec`)
- `GET /api/v1/services/:id`
- `PUT /api/v1/services/:id` (body: `ServiceSpec`)
- `DELETE /api/v1/services/:id`
- `POST /api/v1/services/:id/register`
- `POST /api/v1/services/:id/unregister`
- `POST /api/v1/services/:id/start`
- `POST /api/v1/services/:id/stop`
- `POST /api/v1/services/:id/restart`
- `POST /api/v1/services/:id/repair` (runs the service repair hook when configured; otherwise
  legacy register + restart)
- `GET /api/v1/services/:id/status`
- `GET /api/v1/services/:id/logs?since=&until=&limit=`
- `GET /api/v1/audit?limit=`
- `GET /api/v1/export`
- `POST /api/v1/import` (body: JSON export)

Service list/status filters:

- `tag` / `tags`: exact service tag match. Comma-separated values are allowed and all listed tags
  must match.
- `group` / `groups`: exact group name match. Groups are declared by service tags named
  `group:<name>`, for example `group:phone-control`.
- Filters can be combined. `GET /api/v1/services` with no filters still returns every service.
- `GET /api/v1/services/statuses` returns an array of `{ "service": Service, "status":
  ServiceStatus|null, "error": "" }` objects so one provider status failure does not hide other
  services.
- `GET /api/v1/groups/:name/status` uses the same status item shape and returns `404` when the
  group does not exist.

Error shape:

```json
{ "error": { "code": "bad_request", "message": "..." } }
```

## App Control Surface

The local API is the integration point for internal apps such as SmallPhoneAI and SmallPhone.
Lifecycle control is intentionally bearer-token protected and local by default.

Typical flow:

```bash
TOKEN="$(service-manager token show | sed -n '1p')"

curl -fsS -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/services?group=phone-control

curl -fsS -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/services/statuses?tag=smallphoneai

curl -fsS -X POST -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:20087/api/v1/groups/phone-control/restart
```

`restart` and `repair` are intentionally separate. `restart` only re-registers the service with
its provider and asks the provider to restart it. `repair` runs `spec.repair` when configured and
does not fall back to restart if the hook fails; services without a repair hook keep the legacy
register + restart behavior.

Managed service command contract:

- `spec.command` must start the long-lived foreground process that service-manager should own.
- If `spec.command` points to a wrapper script, the wrapper must finish by `exec`-ing the real
  server process. Do not start the server in the background and then exit the wrapper.
- stdout/stderr from the managed foreground process are captured by the provider logs API or its
  underlying service platform. Prefer writing operational logs there instead of hidden ad-hoc log
  files.
- `health` checks describe whether a running service is usable; they do not replace provider
  lifecycle tracking. A service should be considered healthy only when the tracked process is still
  running and its health checks pass.
- Repair hook stdout/stderr are intentionally discarded. Hook failures are returned by exit status
  only so tokens, credentials, and environment details are not leaked through the API.

CLI support today is for running the server, diagnostics, token management, and installing the
service-manager daemon:

- `service-manager serve`
- `service-manager doctor`
- `service-manager token show`
- `service-manager token rotate`
- `service-manager install-service`
- `service-manager uninstall-service`

Service registration and lifecycle operations are available through the authenticated REST API and
Web UI.

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
