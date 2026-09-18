#!/bin/bash

app_proxy_shell_quote() {
  printf "'"
  printf "%s" "$1" | /usr/bin/sed "s/'/'\\\\''/g"
  printf "'"
}

app_proxy_stable_hash() {
  local app_path="$1"
  local bundle_id="$2"
  printf '%s\n%s\n' "$app_path" "$bundle_id" | /usr/bin/shasum -a 256 | /usr/bin/awk '{print substr($1,1,12)}'
}

app_proxy_normalize_port() {
  local port="$1"
  local normalized
  if [[ ! "$port" =~ ^[0-9]+$ ]]; then return 1; fi
  normalized="$port"
  while [[ "${#normalized}" -gt 1 && "${normalized:0:1}" == "0" ]]; do
    normalized="${normalized:1}"
  done
  if [[ "$normalized" == "0" ]]; then return 1; fi
  if [[ "${#normalized}" -gt 5 ]]; then return 1; fi
  if [[ "${#normalized}" -eq 5 && "$normalized" > "65535" ]]; then return 1; fi
  printf '%s\n' "$normalized"
}

app_proxy_port_available() {
  local port
  local nc_bin="/usr/bin/nc"
  port="$(app_proxy_normalize_port "$1")" || return 1
  if [[ -n "${APP_PROXY_TEST_BIN_DIR:-}" && -x "$APP_PROXY_TEST_BIN_DIR/nc" ]]; then
    nc_bin="$APP_PROXY_TEST_BIN_DIR/nc"
  fi
  ! "$nc_bin" -G 1 -z 127.0.0.1 "$port" >/dev/null 2>&1
}

app_proxy_find_available_port() {
  local port
  port="$(app_proxy_normalize_port "${1:-18099}")" || return 1
  while [[ "$port" -le 65535 ]]; do
    if app_proxy_port_available "$port"; then
      printf '%s\n' "$port"
      return 0
    fi
    port=$((port + 1))
  done
  return 1
}

app_proxy_plist_value() {
  local plist="$1"
  local key="$2"
  /usr/libexec/PlistBuddy -c "Print :$key" "$plist" 2>/dev/null || true
}

app_proxy_app_executable() {
  local app_path="$1"
  local executable_name
  local executable_path
  executable_name="$(app_proxy_plist_value "$app_path/Contents/Info.plist" CFBundleExecutable)"
  if [[ -z "$executable_name" ]]; then return 1; fi
  executable_path="$app_path/Contents/MacOS/$executable_name"
  if [[ ! -x "$executable_path" ]]; then return 1; fi
  printf '%s\n' "$executable_path"
}

app_proxy_display_name() {
  local app_path="$1"
  local plist="$app_path/Contents/Info.plist"
  local name
  name="$(app_proxy_plist_value "$plist" CFBundleDisplayName)"
  if [[ -z "$name" ]]; then name="$(app_proxy_plist_value "$plist" CFBundleName)"; fi
  if [[ -z "$name" ]]; then name="$(/usr/bin/basename "$app_path" .app)"; fi
  printf '%s\n' "$name"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "This V2 resource is not callable directly yet." >&2
  exit 64
fi
