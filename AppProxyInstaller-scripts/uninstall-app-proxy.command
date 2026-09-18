#!/bin/bash
set -Eeuo pipefail

APP_PROXY_ERROR_REPORTED=0
APP_PROXY_SUCCESS_PROMPT_SHOWN=0
APP_PROXY_SUCCESS_EXIT_MESSAGE="All checks passed."

app_proxy_close_terminal_on_success() {
  if [[ "${APP_PROXY_KEEP_TERMINAL_OPEN:-0}" == "1" ]]; then return 0; fi
  local tty_path tty_name
  tty_path="$(/usr/bin/tty 2>/dev/null || true)"
  [[ -n "$tty_path" && "$tty_path" != "not a tty" ]] || return 0
  tty_name="$(/usr/bin/basename "$tty_path")"
  (
    trap '' HUP
    /bin/sleep 0.35
    /usr/bin/osascript >/dev/null 2>&1 <<OSA
    tell application "Terminal"
      repeat with terminalWindow in windows
        repeat with terminalTab in tabs of terminalWindow
          set terminalTty to tty of terminalTab
          if terminalTty is "$tty_path" or terminalTty is "$tty_name" then
            if (count of tabs of terminalWindow) is 1 then
              close terminalWindow
            else
              close terminalTab
            end if
            return
          end if
        end repeat
      end repeat
    end tell
OSA
  ) >/dev/null 2>&1 < /dev/null &
  disown "$!" 2>/dev/null || true
}

app_proxy_wait_before_success_close() {
  if [[ "${APP_PROXY_SUCCESS_PROMPT_SHOWN:-0}" == "1" ]]; then return 0; fi
  APP_PROXY_SUCCESS_PROMPT_SHOWN=1
  if [[ "${APP_PROXY_NONINTERACTIVE:-0}" == "1" ]]; then return 0; fi
  printf '\n%s Press any key to close this window.' "${APP_PROXY_SUCCESS_EXIT_MESSAGE:-All checks passed.}"
  IFS= read -r -n 1 _ || true
  printf '\n'
}

app_proxy_pause_after_error() {
  printf '\nApp Proxy operation did not complete.\n' >&2
  if [[ "${APP_PROXY_NONINTERACTIVE:-0}" == "1" ]]; then return 0; fi
  printf 'Press any key to close this window.' >&2
  IFS= read -r -n 1 _ || true
  printf '\n' >&2
  app_proxy_close_terminal_on_success
}

app_proxy_on_error() {
  local status=$?
  local line_no="${1:-unknown}"
  local command="${2:-unknown}"
  if [[ "$command" == return\ * ]]; then
    return "$status"
  fi
  APP_PROXY_ERROR_REPORTED=1
  printf '\nError at line %s: %s\n' "$line_no" "$command" >&2
  app_proxy_pause_after_error
  exit "$status"
}

app_proxy_on_exit() {
  local status=$?
  if [[ "$status" -eq 0 ]]; then
    app_proxy_wait_before_success_close
    app_proxy_close_terminal_on_success
  elif [[ "$APP_PROXY_ERROR_REPORTED" -eq 0 ]]; then
    app_proxy_pause_after_error
  fi
}

trap 'app_proxy_on_error "$LINENO" "$BASH_COMMAND"' ERR
trap 'app_proxy_on_exit' EXIT

SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE="$SOURCE_DIR/Resources/app-proxy-installer-core.sh"

if [[ ! -f "$CORE" ]]; then
  echo "Missing installer core: $CORE" >&2
  exit 1
fi

. "$CORE"
app_proxy_uninstall_main "$@"
