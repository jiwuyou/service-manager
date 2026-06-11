#!/usr/bin/env sh
set -eu

APP_DIR="/root/projects/service-manager"
BIND_ADDR="127.0.0.1:20087"
URL="http://${BIND_ADDR}/"
LOG_FILE="/tmp/service-manager-20087.log"

cd "$APP_DIR"

if ! curl -fsS "$URL" >/dev/null 2>&1; then
  nohup "$APP_DIR/target/debug/service-manager" serve --bind "$BIND_ADDR" >"$LOG_FILE" 2>&1 &
  sleep 1
fi

exec google-chrome-stable "$URL"
