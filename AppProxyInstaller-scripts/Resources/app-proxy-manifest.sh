#!/bin/bash

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "This manifest resource is meant to be sourced." >&2
  exit 64
fi

APP_PROXY_MANIFEST_RESOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

app_proxy_manifest_path() {
  if [[ -n "${APP_PROXY_MANIFEST:-}" ]]; then
    printf '%s\n' "$APP_PROXY_MANIFEST"
    return 0
  fi
  printf '%s\n' "${APP_PROXY_HOME:-$HOME}/Library/Application Support/App Proxy/singbox-manifest.json"
}

app_proxy_manifest_exists() {
  [[ -f "$(app_proxy_manifest_path)" ]]
}

app_proxy_manifest_jxa() {
  local command="$1"
  shift
  /usr/bin/osascript -l JavaScript "$APP_PROXY_MANIFEST_RESOURCE_DIR/app-proxy-manifest.jxa" "$command" "$(app_proxy_manifest_path)" "$@"
}

app_proxy_manifest_secure() {
  local path
  path="$(app_proxy_manifest_path)"
  if [[ -f "$path" ]]; then
    /bin/chmod 600 "$path"
  fi
}

app_proxy_manifest_init() {
  local dir
  dir="$(/usr/bin/dirname "$(app_proxy_manifest_path)")"
  /bin/mkdir -p "$dir" || return 1
  app_proxy_manifest_jxa init </dev/null || return 1
  app_proxy_manifest_secure
}

app_proxy_manifest_read() {
  app_proxy_manifest_jxa read </dev/null
}

app_proxy_manifest_add_profile() {
  local profile_json="$1"
  local profile_id
  profile_id="$(printf '%s' "$profile_json" | app_proxy_manifest_jxa add-profile)" || return 1
  app_proxy_manifest_secure
  printf '%s\n' "$profile_id"
}

app_proxy_manifest_update_profile() {
  local profile_id="$1"
  local patch_json="$2"
  printf '%s' "$patch_json" | app_proxy_manifest_jxa update-profile "$profile_id" || return 1
  app_proxy_manifest_secure
}

app_proxy_manifest_get_profile() {
  app_proxy_manifest_jxa get-profile "$1" </dev/null
}

app_proxy_manifest_remove_profile() {
  app_proxy_manifest_jxa remove-profile "$1" </dev/null || return 1
  app_proxy_manifest_secure
}

app_proxy_manifest_set_setting() {
  app_proxy_manifest_jxa set-setting "$1" "${2:-}" </dev/null || return 1
  app_proxy_manifest_secure
}

app_proxy_manifest_profile_dependents() {
  app_proxy_manifest_jxa dependents "$1" </dev/null
}

app_proxy_manifest_align_subscription() {
  local profile_id="$1"
  local status
  app_proxy_manifest_jxa align-subscription "$profile_id"
  status=$?
  app_proxy_manifest_secure
  return "$status"
}

app_proxy_manifest_clear_relay() {
  app_proxy_manifest_jxa clear-relay "$1" </dev/null || return 1
  app_proxy_manifest_secure
}

app_proxy_manifest_add_warning() {
  app_proxy_manifest_jxa add-warning "$1" </dev/null || return 1
  app_proxy_manifest_secure
}

app_proxy_manifest_clear_warnings() {
  app_proxy_manifest_jxa clear-warnings </dev/null || return 1
  app_proxy_manifest_secure
}

app_proxy_manifest_import_legacy() {
  local legacy_config="$1"
  local dir
  dir="$(/usr/bin/dirname "$(app_proxy_manifest_path)")"
  /bin/mkdir -p "$dir" || return 1
  app_proxy_manifest_jxa import-legacy "$legacy_config" </dev/null || return 1
  app_proxy_manifest_secure
}
