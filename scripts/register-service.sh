#!/usr/bin/env bash
set -euo pipefail

cat <<'EOF'
service-manager is the control plane for OpenHouse services.

It does not self-register by default. Other services register themselves with
service-manager using the management token.

Next steps:
  - Run: service-manager serve --bind 127.0.0.1:20087
  - Retrieve a token: service-manager token show
EOF

