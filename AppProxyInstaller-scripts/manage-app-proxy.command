#!/bin/bash
set -uo pipefail

SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLI="$SOURCE_DIR/Resources/app-proxy-cli.sh"

if [[ ! -f "$CLI" ]]; then
  echo "Missing CLI resource: $CLI" >&2
  exit 1
fi

# Use the DMG-local resources so this command always matches its own version,
# and refresh the installed CLI copy when one exists (keeps `app-proxy` current).
. "$CLI"
INSTALLED_BIN="${APP_PROXY_HOME:-$HOME}/Library/Application Support/App Proxy/bin/app-proxy"
if [[ -x "$INSTALLED_BIN" ]]; then
  app_proxy_install_cli >/dev/null 2>&1 || true
fi

exit_status=0
app_proxy_cli_main "$@" || exit_status=$?
if [[ "$exit_status" -ne 0 && -t 0 && "${APP_PROXY_NONINTERACTIVE:-0}" != "1" ]]; then
  printf '\nApp Proxy operation did not complete (status %s). Press any key to close this window.' "$exit_status" >&2
  IFS= read -r -n 1 _ || true
  printf '\n' >&2
fi
exit "$exit_status"
