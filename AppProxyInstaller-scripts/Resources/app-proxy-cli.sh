#!/bin/bash

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "This CLI resource is meant to be sourced by the app-proxy entry script." >&2
  exit 64
fi

APP_PROXY_CLI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if ! declare -F app_proxy_home >/dev/null 2>&1; then
  . "$APP_PROXY_CLI_DIR/app-proxy-installer-core.sh"
fi
if ! declare -F app_proxy_manifest_path >/dev/null 2>&1; then
  . "$APP_PROXY_CLI_DIR/app-proxy-manifest.sh"
fi

app_proxy_cli_noninteractive() {
  [[ "${APP_PROXY_NONINTERACTIVE:-0}" == "1" ]]
}

app_proxy_cli_help_requested() {
  local arg
  for arg in "$@"; do
    case "$arg" in
      -h|--help)
        return 0
        ;;
    esac
  done
  return 1
}

app_proxy_cli_json_query() {
  local json="$1"
  local script="$2"
  /usr/bin/osascript -l JavaScript -e "
function run(argv) {
  var data = JSON.parse(argv[0]);
  $script
}
" "$json"
}

# --- interactive selection helper ----------------------------------------------

app_proxy_cli_choose() {
  # $1: title, $2: newline-separated choices; prints the 1-based index on stdout.
  # Arrow-key TUI when a terminal is available, numbered prompt (on stderr) otherwise.
  local title="$1"
  local choices="$2"
  local index=0
  local line
  local answer

  if app_proxy_tui_available; then
    app_proxy_select_single "$title" "$choices"
    return $?
  fi

  echo "$title" >&2
  while IFS= read -r line; do
    index=$((index + 1))
    echo "  $index) $line" >&2
  done <<<"$choices"
  printf 'Number: ' >&2
  if ! IFS= read -r answer; then
    # EOF: no more input is coming; callers must stop prompting
    return 2
  fi
  if [[ ! "$answer" =~ ^[0-9]+$ ]] || [[ -z "$(printf '%s\n' "$choices" | /usr/bin/sed -n "${answer}p")" ]]; then
    echo "invalid selection: $answer" >&2
    return 1
  fi
  printf '%s\n' "$answer"
}

# --- proxy URI parsing -------------------------------------------------------

app_proxy_cli_parse_proxy_uri() {
  local uri="$1"
  local scheme
  local rest
  local userinfo=""
  local hostport
  local user=""
  local pass=""
  local host
  local port
  local tls
  local protocol

  if [[ "$uri" != *"://"* ]]; then
    echo "unsupported proxy URI (expected http(s)://[user:pass@]host:port or socks5://...): $uri" >&2
    return 1
  fi
  scheme="${uri%%://*}"
  case "$scheme" in
    http|https|socks5) ;;
    *)
      echo "unsupported proxy URI scheme '$scheme' (expected http, https, or socks5): $uri" >&2
      return 1
      ;;
  esac

  rest="${uri#*://}"
  rest="${rest%/}"
  if [[ "$rest" == *@* ]]; then
    # credentials may contain @ themselves; the host starts after the LAST @
    hostport="${rest##*@}"
    userinfo="${rest%@*}"
  else
    hostport="$rest"
  fi

  if [[ ! "$hostport" =~ ^([^:@/]+):([0-9]+)$ ]]; then
    echo "unsupported proxy URI (expected host:port after the credentials): $uri" >&2
    return 1
  fi
  host="${BASH_REMATCH[1]}"
  port="$(app_proxy_normalize_port "${BASH_REMATCH[2]}")" || {
    echo "invalid port in proxy URI: $uri" >&2
    return 1
  }

  if [[ -n "$userinfo" ]]; then
    # credentials are taken literally: real proxy providers issue passwords that
    # may themselves contain %XX; percent-decoding would corrupt them
    user="${userinfo%%:*}"
    if [[ "$userinfo" == *:* ]]; then
      pass="${userinfo#*:}"
    fi
  fi

  tls=false
  protocol="$scheme"
  if [[ "$scheme" == "https" ]]; then
    protocol="http"
    tls=true
  fi

  printf '{"protocol":%s,"server":%s,"server_port":%s,"username":%s,"password":%s,"tls":%s}\n' \
    "$(app_proxy_json_string "$protocol")" \
    "$(app_proxy_json_string "$host")" \
    "$port" \
    "$(app_proxy_json_string "$user")" \
    "$(app_proxy_json_string "$pass")" \
    "$tls"
}

# --- rebinding ----------------------------------------------------------------

app_proxy_cli_verify_port_listening() {
  # after a sing-box restart, poll until the given port accepts connections
  local port="$1"
  local attempts="${APP_PROXY_BIND_VERIFY_ATTEMPTS:-5}"
  local attempt

  for ((attempt = 1; attempt <= attempts; attempt++)); do
    if app_proxy_cmd nc -z 127.0.0.1 "$port" >/dev/null 2>&1; then
      return 0
    fi
    [[ "$attempt" -lt "$attempts" ]] && /bin/sleep 1
  done
  return 1
}

app_proxy_cli_apply_verified() {
  # atomic manifest apply: regenerate the sing-box config, restart, and verify
  # that $1 (port) is actually listening. On any failure roll back the manifest
  # to snapshot $2 and the sing-box config to snapshot $3, then restart again
  # so the machine is left in the pre-change state.
  local port="$1"
  local manifest_snapshot="$2"
  local config_snapshot="$3"
  local singbox_config

  if app_proxy_cli_regenerate && app_proxy_cli_verify_port_listening "$port"; then
    return 0
  fi

  echo "sing-box did not come up with port $port listening; rolling back." >&2
  singbox_config="$(app_proxy_singbox_config_path)"
  if [[ -n "$manifest_snapshot" && -f "$manifest_snapshot" ]]; then
    /bin/cp "$manifest_snapshot" "$(app_proxy_manifest_path)" 2>/dev/null || true
    app_proxy_manifest_secure || true
  fi
  if [[ -n "$config_snapshot" && -f "$config_snapshot" ]]; then
    /bin/cp "$config_snapshot" "$singbox_config" 2>/dev/null || true
    app_proxy_restart_singbox >/dev/null 2>&1 || true
  fi
  echo "Rolled back: manifest and sing-box config restored; no app files were touched." >&2
  return 1
}

app_proxy_cli_snapshot_file() {
  # copy $1 to a temp snapshot; prints the snapshot path (empty if $1 missing)
  local source="$1"
  local snapshot

  if [[ ! -f "$source" ]]; then
    printf '\n'
    return 0
  fi
  snapshot="$(/usr/bin/mktemp "${TMPDIR:-/tmp}/app-proxy-snapshot.XXXXXX")" || return 1
  /bin/cp "$source" "$snapshot" || {
    /bin/rm -f "$snapshot"
    return 1
  }
  printf '%s\n' "$snapshot"
}

app_proxy_cli_bind_wrapper() {
  local wrapper_config="$1"
  local profile_id="$2"
  local profile_json
  local port
  local target_app
  local proxy_host
  local display_name
  local bundle_id
  local kind

  if [[ ! -f "$wrapper_config" ]]; then
    echo "wrapper config missing: $wrapper_config" >&2
    return 1
  fi
  profile_json="$(app_proxy_manifest_get_profile "$profile_id")" || return 1
  port="$(app_proxy_json_get "$profile_json" listen_port)"
  if [[ -z "$port" ]]; then
    echo "profile $profile_id has no listen_port" >&2
    return 1
  fi

  target_app="$(app_proxy_cli_wrapper_field "$wrapper_config" TARGET_APP_PATH)"
  proxy_host="$(app_proxy_cli_wrapper_field "$wrapper_config" PROXY_HOST)"
  proxy_host="${proxy_host:-127.0.0.1}"

  local clone_home
  clone_home="$(app_proxy_cli_wrapper_field "$wrapper_config" CLONE_HOME)"

  # atomic order: manifest -> regenerated sing-box config -> sing-box check ->
  # restart -> port listening; only then touch config.env / official configs
  local manifest_snapshot
  local config_snapshot
  manifest_snapshot="$(app_proxy_cli_snapshot_file "$(app_proxy_manifest_path)")" || return 1
  config_snapshot="$(app_proxy_cli_snapshot_file "$(app_proxy_singbox_config_path)")" || {
    /bin/rm -f "$manifest_snapshot"
    return 1
  }

  kind=""
  if [[ -d "$target_app" ]]; then
    display_name="$(app_proxy_display_name "$target_app")"
    bundle_id="$(app_proxy_plist_value "$target_app/Contents/Info.plist" CFBundleIdentifier)"
    kind="$(app_proxy_target_kind "$target_app" "$display_name" "$bundle_id")"
  fi
  if [[ "$kind" == "claude" && -z "$clone_home" ]]; then
    if ! app_proxy_manifest_set_setting claude_rules_profile "$profile_id"; then
      /bin/rm -f "$manifest_snapshot" "$config_snapshot"
      return 1
    fi
  fi

  if ! app_proxy_cli_apply_verified "$port" "$manifest_snapshot" "$config_snapshot"; then
    /bin/rm -f "$manifest_snapshot" "$config_snapshot"
    return 1
  fi
  /bin/rm -f "$manifest_snapshot" "$config_snapshot"

  if /usr/bin/grep -q '^PROXY_PORT=' "$wrapper_config"; then
    /usr/bin/sed -i '' "s|^PROXY_PORT=.*|PROXY_PORT=$port|" "$wrapper_config" || return 1
  else
    {
      printf 'PROXY_SCHEME=http\n'
      printf 'PROXY_HOST=%s\n' "$(app_proxy_shell_quote "$proxy_host")"
      printf 'PROXY_PORT=%s\n' "$port"
    } >>"$wrapper_config" || return 1
  fi

  if [[ -d "$target_app" ]]; then
    if [[ -n "$clone_home" ]]; then
      ( APP_PROXY_HOME="$clone_home" app_proxy_configure_target_official_proxy "$target_app" "$display_name" "$bundle_id" "$proxy_host" "$port" ) || return $?
    else
      app_proxy_configure_target_official_proxy "$target_app" "$display_name" "$bundle_id" "$proxy_host" "$port" || return $?
    fi
  else
    echo "Warning: target app missing on disk, only wrapper config updated: $target_app" >&2
  fi

  echo "Rebound $(/usr/bin/basename "$(/usr/bin/dirname "$wrapper_config")") to profile $profile_id (port $port)."
}

# --- delete port ---------------------------------------------------------------

app_proxy_cli_delete_port() {
  local profile_id="$1"
  shift 2>/dev/null || true
  local cascade=0
  local arg
  local profile_json
  local port
  local deps_json
  local relay_count
  local claude_target
  local wrappers
  local profile_count
  local answer
  local config
  local cleared

  local relay_mode="direct"
  local relay_profiles
  local dep_id

  for arg in "$@"; do
    case "$arg" in
      --cascade)
        cascade=1
        ;;
      --cascade-delete-dependents)
        cascade=1
        relay_mode="delete"
        ;;
    esac
  done

  # cycle guard for recursive dependent deletion
  case " ${APP_PROXY_CASCADE_VISITED:-} " in
    *" $profile_id "*)
      return 0
      ;;
  esac

  profile_json="$(app_proxy_manifest_get_profile "$profile_id")" || return 1
  port="$(app_proxy_json_get "$profile_json" listen_port)"
  deps_json="$(app_proxy_manifest_profile_dependents "$profile_id")" || return 1
  relay_count="$(app_proxy_cli_json_query "$deps_json" 'return String(data.relay_dependents.length);')"
  claude_target="$(app_proxy_cli_json_query "$deps_json" 'return String(data.claude_rules_target);')"
  relay_profiles="$(app_proxy_cli_json_query "$deps_json" 'var seen = {}; return data.relay_dependents.map(function(d){return d.profile;}).filter(function(p){ if (seen[p]) { return false; } seen[p] = true; return true; }).join("\n");')"
  wrappers="$(app_proxy_cli_wrapper_dependents "$port")"

  if [[ "$relay_count" != "0" || "$claude_target" == "true" || -n "$wrappers" ]]; then
    echo "Profile $profile_id (port $port) has dependents:" >&2
    if [[ -n "$wrappers" ]]; then
      printf '%s\n' "$wrappers" | while IFS= read -r name; do
        [[ -n "$name" ]] && echo "  · app bound to this port: $name (will be unbound and its wrapper removed)" >&2
      done
    fi
    if [[ "$relay_count" != "0" ]]; then
      echo "  · relay dependents: $(app_proxy_cli_json_query "$deps_json" 'return data.relay_dependents.map(function(d){return d.profile + "/" + d.node;}).join(", ");') (these profiles relay THROUGH $profile_id and will stop working as-is)" >&2
    fi
    if [[ "$claude_target" == "true" ]]; then
      echo "  · Claude domain rules point at this profile (rules will be removed)" >&2
    fi

    if [[ "$cascade" -ne 1 ]]; then
      if app_proxy_cli_noninteractive; then
        echo "Re-run with --cascade (relay dependents fall back to direct) or --cascade-delete-dependents (delete them and their bindings too), or rebind first (app-proxy bind / app-proxy node)." >&2
        return 1
      fi
      if [[ "$relay_count" != "0" ]]; then
        answer="$(app_proxy_cli_choose "Delete $profile_id anyway? Its relay dependents must be handled:" \
          $'Convert relay dependents to DIRECT connection (keep them), then delete\nDelete the relay dependents too, including their bound apps/wrappers/guards\nCancel')" || return 1
        case "$answer" in
          1) cascade=1; relay_mode="direct" ;;
          2) cascade=1; relay_mode="delete" ;;
          *) echo "Cancelled; nothing changed."; return 1 ;;
        esac
      else
        answer="$(app_proxy_cli_choose "Delete $profile_id anyway?" \
          $'Cascade: clean up the dependents listed above, then delete\nCancel')" || return 1
        if [[ "$answer" != "1" ]]; then
          echo "Cancelled; nothing changed."
          return 1
        fi
        cascade=1
      fi
    fi
  fi

  if [[ "${APP_PROXY_CASCADE_INNER:-0}" != "1" ]]; then
    local chain_size=1
    if [[ "$relay_mode" == "delete" && -n "$relay_profiles" ]]; then
      chain_size=$((chain_size + $(printf '%s\n' "$relay_profiles" | /usr/bin/grep -c .)))
    fi
    profile_count="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return String(data.profiles.length);')"
    if [[ $((profile_count - chain_size)) -lt 1 ]]; then
      echo "Refusing: this deletion would remove every remaining profile; use the uninstaller to remove the sing-box environment." >&2
      return 1
    fi
  fi

  if [[ "$cascade" -eq 1 ]]; then
    if [[ "$relay_mode" == "delete" && -n "$relay_profiles" ]]; then
      while IFS= read -r dep_id; do
        [[ -n "$dep_id" ]] || continue
        echo "Deleting relay-dependent profile $dep_id (and its bindings)..."
        APP_PROXY_CASCADE_VISITED="${APP_PROXY_CASCADE_VISITED:-} $profile_id" \
          APP_PROXY_CASCADE_INNER=1 \
          app_proxy_cli_delete_port "$dep_id" --cascade-delete-dependents || return 1
      done <<<"$relay_profiles"
    elif [[ "$relay_count" != "0" ]]; then
      cleared="$(app_proxy_manifest_clear_relay "$profile_id")" || return 1
      echo "Cleared relay references on $cleared node(s); they now connect directly."
    fi

    while IFS= read -r config; do
      [[ -n "$config" ]] || continue
      if [[ -n "$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)" ]]; then
        APP_PROXY_UNINSTALL_PURGE_CLONES=0 app_proxy_uninstall_clone_by_config "$config" || return 1
      else
        app_proxy_uninstall_proxy_app "$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)" || return 1
      fi
    done < <(app_proxy_cli_wrapper_dependent_configs "$port")

    if [[ "$claude_target" == "true" ]]; then
      app_proxy_manifest_set_setting claude_rules_profile "" || return 1
      echo "Claude domain rules target cleared."
    fi
  fi

  app_proxy_manifest_remove_profile "$profile_id" || return 1
  echo "Profile $profile_id removed."
  if [[ "${APP_PROXY_CASCADE_INNER:-0}" == "1" ]]; then
    return 0
  fi
  app_proxy_cli_regenerate
}

# --- subscription update -------------------------------------------------------

app_proxy_cli_subscription_profiles() {
  app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return data.profiles.filter(function(p){return p.source && p.source.kind === "subscription";}).map(function(p){return p.id;}).join("\n");'
}

app_proxy_cli_reselect_profile_nodes() {
  local profile_id="$1"
  local parsed_file="$2"
  local country_index
  local node_indexes
  local country_choices
  local node_choices
  local country_list
  local node_list
  local nodes_json

  echo "Choose replacement nodes for profile $profile_id:"
  if app_proxy_tui_available; then
    country_choices="$(app_proxy_country_choices "$parsed_file")" || return 1
    country_index="$(app_proxy_select_single "Available countries:" "$country_choices")" || return 1
    node_choices="$(app_proxy_node_choices_for_country "$parsed_file" "$country_index")" || return 1
    node_indexes="$(app_proxy_select_multi "Available nodes:" "$node_choices")" || return 1
  else
    country_list="$(app_proxy_country_list "$parsed_file")" || return 1
    echo "Available countries:"
    printf '%s\n' "$country_list"
    printf 'Choose country number: '
    IFS= read -r country_index || country_index=""
    node_list="$(app_proxy_nodes_for_country "$parsed_file" "$country_index")" || return 1
    echo "Available nodes:"
    printf '%s\n' "$node_list"
    printf 'Choose node numbers, comma-separated: '
    IFS= read -r node_indexes || node_indexes=""
  fi

  nodes_json="$(app_proxy_cli_nodes_json_from_parsed "$parsed_file" "$country_index" "$node_indexes")" || return 1
  app_proxy_manifest_update_profile "$profile_id" "{\"nodes\": $nodes_json}" || return 1
}

app_proxy_cli_update_profile() {
  local profile_id="$1"
  local non_interactive="$2"
  local profile_json
  local kind
  local url
  local tmp_dir
  local diff_json
  local align_status
  local removed_names

  profile_json="$(app_proxy_manifest_get_profile "$profile_id")" || return 1
  kind="$(app_proxy_cli_json_query "$profile_json" 'return data.source ? String(data.source.kind || "") : "";')"
  if [[ "$kind" != "subscription" ]]; then
    echo "profile $profile_id is not subscription-backed (source: ${kind:-none}); update unavailable" >&2
    return 1
  fi
  url="$(app_proxy_cli_json_query "$profile_json" 'return String(data.source.url || "");')"

  tmp_dir="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/app-proxy-update.XXXXXX")" || return 1

  if ! app_proxy_download_subscription "$url" "$tmp_dir/subscription.txt"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi
  if ! /usr/bin/osascript -l JavaScript "$APP_PROXY_CLI_DIR/app-proxy-subscription-parser.jxa" <"$tmp_dir/subscription.txt" >"$tmp_dir/parsed.json"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi

  set +e
  diff_json="$(app_proxy_manifest_align_subscription "$profile_id" <"$tmp_dir/parsed.json")"
  align_status=$?
  set -e

  if [[ "$align_status" -eq 0 ]]; then
    echo "Subscription update for $profile_id:"
    echo "  kept: $(app_proxy_cli_json_query "$diff_json" 'return String(data.kept);')  added: $(app_proxy_cli_json_query "$diff_json" 'return String(data.added);')  removed selected: $(app_proxy_cli_json_query "$diff_json" 'return String(data.removed_selected);')"
    removed_names="$(app_proxy_cli_json_query "$diff_json" 'return data.removed_selected_names.join(", ");')"
    if [[ -n "$removed_names" ]]; then
      echo "  removed from egress group: $removed_names"
    fi
    /bin/rm -rf "$tmp_dir"
    app_proxy_cli_regenerate
    return $?
  fi

  if [[ "$align_status" -eq 3 ]]; then
    echo "Subscription update for $profile_id would empty its egress group." >&2
    if [[ "$non_interactive" == "1" ]]; then
      app_proxy_manifest_add_warning "subscription update for $profile_id would empty its egress group; node reselection required (app-proxy update $profile_id)"
      echo "Left configuration unchanged; recorded a pending warning." >&2
      /bin/rm -rf "$tmp_dir"
      return 1
    fi
    if ! app_proxy_cli_reselect_profile_nodes "$profile_id" "$tmp_dir/parsed.json"; then
      /bin/rm -rf "$tmp_dir"
      return 1
    fi
    /bin/rm -rf "$tmp_dir"
    app_proxy_cli_regenerate
    return $?
  fi

  /bin/rm -rf "$tmp_dir"
  return "$align_status"
}

app_proxy_cli_update() {
  local non_interactive=0
  local all=0
  local targets=()
  local arg
  local id
  local overall=0

  for arg in "$@"; do
    case "$arg" in
      --non-interactive) non_interactive=1 ;;
      --all) all=1 ;;
      *) targets+=("$arg") ;;
    esac
  done
  if app_proxy_cli_noninteractive; then
    non_interactive=1
  fi

  if [[ "$all" -eq 1 || "${#targets[@]}" -eq 0 ]]; then
    while IFS= read -r id; do
      [[ -n "$id" ]] || continue
      targets+=("$id")
    done < <(app_proxy_cli_subscription_profiles)
  fi

  if [[ "${#targets[@]}" -eq 0 ]]; then
    echo "no subscription-backed profiles to update"
    return 0
  fi

  for id in "${targets[@]}"; do
    if ! app_proxy_cli_update_profile "$id" "$non_interactive"; then
      overall=1
    fi
  done
  return "$overall"
}

# --- connectivity test -----------------------------------------------------------

app_proxy_cli_test_dns_report() {
  local config_path
  config_path="$(app_proxy_singbox_config_path)"
  if [[ ! -f "$config_path" ]]; then
    echo "  (sing-box config missing: $config_path)"
    return 0
  fi
  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");
function run(argv) {
  try {
    var text = $.NSString.stringWithContentsOfFileEncodingError(argv[0], $.NSUTF8StringEncoding, null);
    if (!text || (typeof text.isNil === "function" && text.isNil())) return "  (unreadable config)";
    var config = JSON.parse(ObjC.unwrap(text));
    var servers = (config.dns && config.dns.servers) || [];
    var lines = [];
    servers.forEach(function (server) {
      if (server.type === "https") {
        var port = server.server_port && server.server_port !== 443 ? ":" + server.server_port : "";
        lines.push("  " + server.tag + ": https://" + server.server + port + (server.path || ""));
      } else if (server.type === "local") {
        lines.push("  " + server.tag + ": system resolver (bootstrap only)");
      }
    });
    return lines.length ? lines.join("\n") : "  (no DNS servers configured)";
  } catch (error) {
    return "  (unreadable config)";
  }
}
' "$config_path"
}

app_proxy_cli_test() {
  local targets=()
  local arg
  local id
  local overall=0
  local profile_json
  local port
  local kind
  local exit_ip
  local status

  for arg in "$@"; do
    targets+=("$arg")
  done
  if [[ "${#targets[@]}" -eq 0 ]]; then
    while IFS= read -r id; do
      [[ -n "$id" ]] || continue
      targets+=("$id")
    done < <(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return data.profiles.map(function(p){return p.id;}).join("\n");')
  fi
  if [[ "${#targets[@]}" -eq 0 ]]; then
    echo "no profiles configured"
    return 0
  fi

  echo "DNS (from the live sing-box config):"
  app_proxy_cli_test_dns_report
  echo

  for id in "${targets[@]}"; do
    if ! profile_json="$(app_proxy_manifest_get_profile "$id" 2>/dev/null)"; then
      echo "$id: profile not found" >&2
      overall=1
      continue
    fi
    port="$(app_proxy_json_get "$profile_json" listen_port)"
    kind="$(app_proxy_cli_json_query "$profile_json" 'return data.source ? String(data.source.kind || "") : "";')"
    echo "$id  http://127.0.0.1:$port  [$kind]"

    if ! app_proxy_cmd nc -z 127.0.0.1 "$port" >/dev/null 2>&1; then
      echo "  inbound: NOT listening (is sing-box running? brew services restart sing-box)" >&2
      overall=1
      continue
    fi
    echo "  inbound: listening"

    exit_ip="$(app_proxy_cmd curl -x "http://127.0.0.1:$port" -fsS --max-time 20 https://ifconfig.me 2>/dev/null || true)"
    exit_ip="${exit_ip//$'\n'/}"
    exit_ip="${exit_ip//$'\r'/}"
    if [[ -n "$exit_ip" ]]; then
      echo "  exit IP: $exit_ip"
    else
      echo "  exit IP: FAILED (no egress through this port)" >&2
      overall=1
    fi

    if status="$(app_proxy_proxy_http_status "$port" "https://www.gstatic.com/generate_204")"; then
      echo "  connectivity: HTTP $status (https://www.gstatic.com/generate_204)"
    else
      echo "  connectivity: FAILED (https://www.gstatic.com/generate_204)" >&2
      overall=1
    fi
  done
  return "$overall"
}

# --- status --------------------------------------------------------------------

app_proxy_cli_status() {
  local manifest_json
  local bindings

  manifest_json="$(app_proxy_manifest_read)" || return 1

  echo "Profiles:"
  app_proxy_cli_json_query "$manifest_json" '
    var lines = [];
    data.profiles.forEach(function (profile) {
      var selected = (profile.nodes || []).filter(function (n) { return n.selected; });
      var source = profile.source ? profile.source.kind : "unknown";
      if (profile.source && profile.source.url) {
        source += " " + profile.source.url;
      }
      lines.push("  " + profile.id + "  port " + profile.listen_port + "  [" + source + "]  selected nodes: " + selected.map(function (n) { return n.name; }).join(", "));
    });
    if (data.settings && data.settings.claude_rules_profile) {
      lines.push("  Claude domain rules -> " + data.settings.claude_rules_profile + "  (only applies to traffic not bound to a profile port)");
    }
    (data.pending_warnings || []).forEach(function (warning) {
      lines.push("  [!] " + warning.message);
    });
    return lines.join("\n");
  '

  echo "Wrapper bindings:"
  bindings="$(app_proxy_cli_list_bindings)"
  if [[ -n "$bindings" ]]; then
    printf '%s\n' "$bindings" | while IFS=$'\t' read -r name port target; do
      echo "  $name  -> port ${port:-direct}  ($target)"
    done
  else
    echo "  (none)"
  fi

  echo "Clones:"
  local clones
  clones="$(app_proxy_clone_list)"
  if [[ -n "$clones" ]]; then
    printf '%s\n' "$clones" | while IFS=$'\t' read -r clone_name display mode port target clone_home; do
      echo "  $clone_name  [$mode]  ($display)  -> port $port  data: $clone_home"
    done
  else
    echo "  (none)"
  fi
}

# --- manifest bootstrap / legacy adoption ---------------------------------------

app_proxy_cli_ensure_manifest() {
  local config
  local answer
  local report

  if app_proxy_manifest_exists; then
    return 0
  fi

  config="$(app_proxy_singbox_config_path)"
  if [[ ! -f "$config" ]]; then
    app_proxy_manifest_init
    return 0
  fi

  echo "Detected an existing sing-box config without an app-proxy manifest: $config"
  if app_proxy_cli_noninteractive; then
    echo "Run app-proxy interactively once to import or replace it." >&2
    return 1
  fi

  answer="$(app_proxy_cli_choose "Options:" \
    $'Import and take over (imported nodes cannot be subscription-updated)\nQuit and leave everything unchanged\nIgnore the existing config and start with an empty managed setup (overwrites on next apply)')" || return 70
  case "$answer" in
    1|"")
      if report="$(app_proxy_manifest_import_legacy "$config")"; then
        echo "Imported: $(app_proxy_cli_json_query "$report" 'return "profile " + data.profile + ", port " + data.listen_port + ", " + data.nodes + " node(s), unrecognized rules dropped: " + data.unrecognized_rules;')"
        return 0
      fi
      echo "Import failed; the existing config could not be recognized." >&2
      return 1
      ;;
    3)
      app_proxy_manifest_init
      return 0
      ;;
    *)
      echo "Aborted; nothing changed."
      return 70
      ;;
  esac
}

# --- interactive flows -----------------------------------------------------------

app_proxy_cli_prompt_profile() {
  local prompt="${1:-Choose profile:}"
  local ids
  local choices
  local index
  local id

  ids="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return data.profiles.map(function(p){return p.id + "\tport " + p.listen_port + "\t" + (p.source ? p.source.kind : "");}).join("\n");')"
  if [[ -z "$ids" ]]; then
    echo "no profiles configured" >&2
    return 1
  fi
  choices="$(printf '%s\n' "$ids" | /usr/bin/awk -F'\t' '{print $1"  "$2"  ["$3"]"}')"
  index="$(app_proxy_cli_choose "$prompt" "$choices")" || return 1
  id="$(printf '%s\n' "$ids" | /usr/bin/sed -n "${index}p" | /usr/bin/cut -f1)"
  if [[ -z "$id" ]]; then
    echo "invalid selection" >&2
    return 1
  fi
  printf '%s\n' "$id"
}

app_proxy_cli_find_free_port() {
  local used_ports
  local candidate=18099

  used_ports="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return data.profiles.map(function(p){return String(p.listen_port);}).join(" ");')"
  while [[ "$candidate" -le 65535 ]]; do
    if [[ " $used_ports " != *" $candidate "* ]] && app_proxy_port_available "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi
    candidate=$((candidate + 1))
  done
  echo "no free local port found" >&2
  return 1
}

app_proxy_cli_prompt_manual_node() {
  # prints a node JSON object on stdout; all prompts go to stderr. Relay handled by caller.
  local uri
  local node_json
  local protocol
  local server
  local port
  local username
  local password
  local tls=false
  local index

  {
    echo "Manual proxy node. Accepted URI formats:"
    echo "  http://host:port"
    echo "  http://user:pass@host:port     (password taken literally; may contain @ = %)"
    echo "  https://host:port              (TLS proxy)"
    echo "  socks5://[user:pass@]host:port"
    printf 'Paste the proxy URI (or press Return to enter fields one by one): '
  } >&2
  IFS= read -r uri || uri=""
  if [[ -n "$uri" ]]; then
    if node_json="$(app_proxy_cli_parse_proxy_uri "$uri")"; then
      printf '%s\n' "$node_json"
      return 0
    fi
    echo "Could not parse the URI; falling back to field-by-field entry." >&2
  fi

  index="$(app_proxy_cli_choose "Protocol:" $'http\nsocks5')" || return 1
  if [[ "$index" == "2" ]]; then
    protocol="socks5"
  else
    protocol="http"
  fi
  printf 'Server host: ' >&2
  IFS= read -r server || server=""
  if [[ -z "$server" ]]; then
    echo "server host is required" >&2
    return 1
  fi
  printf 'Server port: ' >&2
  IFS= read -r port || port=""
  port="$(app_proxy_normalize_port "$port")" || {
    echo "invalid server port" >&2
    return 1
  }
  printf 'Username [optional]: ' >&2
  IFS= read -r username || username=""
  printf 'Password [optional]: ' >&2
  IFS= read -r password || password=""
  if [[ "$protocol" == "http" ]]; then
    if app_proxy_prompt_yes_no_default_no "Use TLS (https proxy)?" >&2; then
      tls=true
    fi
  fi

  printf '{"protocol":%s,"server":%s,"server_port":%s,"username":%s,"password":%s,"tls":%s}\n' \
    "$(app_proxy_json_string "$protocol")" \
    "$(app_proxy_json_string "$server")" \
    "$port" \
    "$(app_proxy_json_string "$username")" \
    "$(app_proxy_json_string "$password")" \
    "$tls"
}

app_proxy_cli_subscription_nodes_interactive() {
  # $1: subscription URL; prints nodes JSON array with selection applied
  local url="$1"
  local tmp_dir
  local country_index
  local node_indexes
  local country_choices
  local node_choices
  local country_list
  local node_list
  local nodes_json

  tmp_dir="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/app-proxy-port-add.XXXXXX")" || return 1
  if ! app_proxy_download_subscription "$url" "$tmp_dir/subscription.txt"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi
  if ! /usr/bin/osascript -l JavaScript "$APP_PROXY_CLI_DIR/app-proxy-subscription-parser.jxa" <"$tmp_dir/subscription.txt" >"$tmp_dir/parsed.json"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi

  if app_proxy_tui_available; then
    country_choices="$(app_proxy_country_choices "$tmp_dir/parsed.json")" || { /bin/rm -rf "$tmp_dir"; return 1; }
    country_index="$(app_proxy_select_single "Available countries:" "$country_choices")" || { /bin/rm -rf "$tmp_dir"; return 1; }
    node_choices="$(app_proxy_node_choices_for_country "$tmp_dir/parsed.json" "$country_index")" || { /bin/rm -rf "$tmp_dir"; return 1; }
    node_indexes="$(app_proxy_select_multi "Available nodes:" "$node_choices")" || { /bin/rm -rf "$tmp_dir"; return 1; }
  else
    country_list="$(app_proxy_country_list "$tmp_dir/parsed.json")" || { /bin/rm -rf "$tmp_dir"; return 1; }
    echo "Available countries:" >&2
    printf '%s\n' "$country_list" >&2
    printf 'Choose country number: ' >&2
    IFS= read -r country_index || country_index=""
    node_list="$(app_proxy_nodes_for_country "$tmp_dir/parsed.json" "$country_index")" || { /bin/rm -rf "$tmp_dir"; return 1; }
    echo "Available nodes:" >&2
    printf '%s\n' "$node_list" >&2
    printf 'Choose node numbers, comma-separated: ' >&2
    IFS= read -r node_indexes || node_indexes=""
  fi

  nodes_json="$(app_proxy_cli_nodes_json_from_parsed "$tmp_dir/parsed.json" "$country_index" "$node_indexes")" || { /bin/rm -rf "$tmp_dir"; return 1; }
  /bin/rm -rf "$tmp_dir"
  printf '%s\n' "$nodes_json"
}

app_proxy_cli_add_subscription_profile_interactive() {
  # $1: optional prompt label; $2: "--auto-port" to skip the port prompt (relay use)
  # prints new profile id on stdout, prompts on stderr
  local label="${1:-Subscription URL: }"
  local port_mode="${2:-}"
  local url
  local nodes_json
  local port
  local port_input
  local profile_id

  printf '%s' "$label" >&2
  if ! IFS= read -r url || [[ -z "$url" ]]; then
    echo "subscription URL is required" >&2
    return 1
  fi
  nodes_json="$(app_proxy_cli_subscription_nodes_interactive "$url")" || return 1

  port="$(app_proxy_cli_find_free_port)" || return 1
  if [[ "$port_mode" == "--auto-port" ]]; then
    echo "Relay profile port auto-assigned: 127.0.0.1:$port (relay only; you never need to use it directly)" >&2
  else
    printf 'Listen port [%s]: ' "$port" >&2
    IFS= read -r port_input || port_input=""
    port="${port_input:-$port}"
    port="$(app_proxy_normalize_port "$port")" || {
      echo "invalid listen port" >&2
      return 1
    }
  fi

  profile_id="$(app_proxy_manifest_add_profile "{\"listen_port\": $port, \"source\": {\"kind\": \"subscription\", \"url\": $(app_proxy_json_string "$url")}, \"nodes\": $nodes_json}")" || return 1
  printf '%s\n' "$profile_id"
}

app_proxy_cli_relay_profile_incompatible() {
  # true when the profile's selected egress uses XTLS Vision/REALITY (cannot relay)
  local profile_id="$1"
  local result
  result="$(app_proxy_cli_json_query "$(app_proxy_manifest_get_profile "$profile_id")" '
    function bad(n) {
      var q = n.query || {};
      var flow = String(q.flow || "");
      if (/vision/i.test(flow)) { return true; }
      var sec = String(q.security || "").toLowerCase();
      if (sec === "reality") { return true; }
      if (q.pbk || q["public-key"] || q.publicKey || q.public_key) { return true; }
      return false;
    }
    return (data.nodes || []).some(function (n) { return n.selected && bad(n); }) ? "1" : "0";
  ')"
  [[ "$result" == "1" ]]
}

app_proxy_cli_prompt_relay() {
  # prints relay profile id on stdout, or empty for none; prompts go to stderr
  local index
  local relay_id

  {
    echo "--- Relay ---"
    echo "A relay routes this node's traffic through ANOTHER configured line first,"
    echo "and only then on to the proxy server you entered."
    echo "Pick a relay only when the proxy server is unreachable (or slow) if connected directly."
    echo "Note: the relay line must not be an XTLS Vision/REALITY node (protocol limitation)."
  } >&2
  index="$(app_proxy_cli_choose "Relay for this node?" \
    $'No relay - connect directly (most common)\nRelay through an existing port profile egress (pick from your configured ports)\nAdd a new subscription and use it as the relay (creates a new port profile)')" || return 1
  case "$index" in
    2)
      relay_id="$(app_proxy_cli_prompt_profile "Choose relay profile:")" || return 1
      if app_proxy_cli_relay_profile_incompatible "$relay_id"; then
        echo "Warning: profile $relay_id's selected egress uses XTLS Vision/REALITY, which cannot carry a relay (traffic will time out)." >&2
        echo "Pick a profile whose egress is anytls/trojan/ss/plain vless instead, or choose 'No relay'." >&2
        return 1
      fi
      printf '%s\n' "$relay_id"
      ;;
    3)
      relay_id="$(app_proxy_cli_add_subscription_profile_interactive "Relay subscription URL: " --auto-port)" || return 1
      echo "Relay profile $relay_id created. Continuing with your exit node..." >&2
      printf '%s\n' "$relay_id"
      ;;
    *)
      printf '\n'
      ;;
  esac
}

app_proxy_cli_post_add_binding_menu() {
  local profile_id="$1"
  local answer
  local target_app
  local port
  local bindings
  local config
  local index=0
  local pick
  local chosen

  local profile_json
  local exit_summary
  profile_json="$(app_proxy_manifest_get_profile "$profile_id")"
  port="$(app_proxy_json_get "$profile_json" listen_port)"
  exit_summary="$(app_proxy_cli_json_query "$profile_json" '
    var selected = (data.nodes || []).filter(function (n) { return n.selected; });
    if (data.source && data.source.kind === "manual" && selected.length) {
      var n = selected[0];
      var cred = n.username ? n.username + "@" : "";
      return "exit: " + cred + n.server + ":" + n.server_port;
    }
    return "exit: " + selected.map(function (n) { return n.name; }).join(", ");
  ')"
  echo
  answer="$(app_proxy_cli_choose "Profile $profile_id (port $port, $exit_summary) is ready. Bind an app now?" \
    $'Create a proxy wrapper for a new app (drag the .app here)\nRebind an existing proxy wrapper to this profile\nSkip')" || return 1
  case "$answer" in
    1)
      echo "Drag the target .app here, then press Return:"
      if ! IFS= read -r target_app; then
        return 1
      fi
      target_app="$(app_proxy_trim_dragged_path "$target_app")"
      if [[ -z "$target_app" ]]; then
        echo "app path is empty" >&2
        return 1
      fi
      # a clone injects its own proxy; binding it means updating ITS config,
      # never wrapping it in another proxy app
      if [[ "$(app_proxy_plist_value "$target_app/Contents/Info.plist" CFBundleIdentifier 2>/dev/null)" == local.app-proxy.clone.* ]]; then
        local clone_config
        clone_config="$(app_proxy_home)/Library/Application Support/App Proxy/$(app_proxy_plist_value "$target_app/Contents/Info.plist" CFBundleName)/config.env"
        if [[ ! -f "$clone_config" ]]; then
          echo "clone config missing: $clone_config" >&2
          return 1
        fi
        echo "Target is a clone; rebinding the clone itself (no extra wrapper)."
        app_proxy_cli_bind_wrapper "$clone_config" "$profile_id"
        return $?
      fi
      # atomic order: manifest/sing-box first, wrapper files only once the
      # port is verified listening
      local manifest_snapshot
      local config_snapshot
      manifest_snapshot="$(app_proxy_cli_snapshot_file "$(app_proxy_manifest_path)")" || return 1
      config_snapshot="$(app_proxy_cli_snapshot_file "$(app_proxy_singbox_config_path)")" || {
        /bin/rm -f "$manifest_snapshot"
        return 1
      }
      if [[ "$(app_proxy_detect_target_kind "$target_app")" == "claude" ]]; then
        if ! app_proxy_manifest_set_setting claude_rules_profile "$profile_id"; then
          /bin/rm -f "$manifest_snapshot" "$config_snapshot"
          return 1
        fi
      fi
      if ! app_proxy_cli_apply_verified "$port" "$manifest_snapshot" "$config_snapshot"; then
        /bin/rm -f "$manifest_snapshot" "$config_snapshot"
        return 1
      fi
      /bin/rm -f "$manifest_snapshot" "$config_snapshot"
      app_proxy_generate_wrapper "$target_app" "127.0.0.1" "$port" || return $?
      echo "Proxy wrapper generated for: $target_app"
      ;;
    2)
      bindings="$(app_proxy_cli_wrapper_config_paths)"
      if [[ -z "$bindings" ]]; then
        echo "no existing proxy wrappers found" >&2
        return 1
      fi
      local binding_labels=""
      while IFS= read -r config; do
        binding_labels+="$(/usr/bin/basename "$(/usr/bin/dirname "$config")") (port $(app_proxy_cli_wrapper_field "$config" PROXY_PORT))"$'\n'
      done <<<"$bindings"
      pick="$(app_proxy_cli_choose "Existing wrappers:" "${binding_labels%$'\n'}")" || return 1
      chosen="$(printf '%s\n' "$bindings" | /usr/bin/sed -n "${pick}p")"
      if [[ -z "$chosen" ]]; then
        echo "invalid selection" >&2
        return 1
      fi
      app_proxy_cli_bind_wrapper "$chosen" "$profile_id" || return $?
      ;;
    *)
      echo "Skipped binding."
      ;;
  esac
}

app_proxy_cli_maybe_prompt_doh() {
  # first-time hint: the global DoH server applies to the whole sing-box
  # instance; asked once, then recorded so port add stays lean
  local current
  local prompted
  local input

  current="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return String((data.settings || {}).doh_server || "");')"
  prompted="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return String((data.settings || {}).doh_prompted || "");')"
  if [[ -n "$current" || "$prompted" == "1" ]]; then
    return 0
  fi

  printf 'Global DoH server for all ports [Enter = built-in Google/Cloudflare/AliDNS]: ' >&2
  IFS= read -r input || input=""
  if [[ -n "$input" ]]; then
    app_proxy_manifest_set_setting doh_server "$input" || return 1
  fi
  app_proxy_manifest_set_setting doh_prompted 1 || return 1
}

app_proxy_cli_maybe_prompt_interface() {
  # first-time upstream interface detection, mirroring the original installer:
  # bind node connections to an active physical NIC, fall back to auto binding
  local current
  local prompted

  current="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return String((data.settings || {}).upstream_interface || "");')"
  prompted="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return String((data.settings || {}).interface_prompted || "");')"
  if [[ -n "$current" || "$prompted" == "1" ]]; then
    return 0
  fi

  app_proxy_choose_upstream_interface || return 1
  app_proxy_manifest_set_setting upstream_interface "$APP_PROXY_UPSTREAM_INTERFACE" || return 1
  app_proxy_manifest_set_setting upstream_interface_mode "$APP_PROXY_UPSTREAM_INTERFACE_MODE" || return 1
  app_proxy_manifest_set_setting interface_prompted 1 || return 1
  echo "Upstream interface: ${APP_PROXY_UPSTREAM_INTERFACE:-Auto binding} (change later with: app-proxy settings interface)" >&2
}

app_proxy_cli_settings() {
  local sub="${1:-}"
  local value
  local current_doh
  local current_iface
  local answer

  app_proxy_cli_ensure_manifest || return $?

  case "$sub" in
    doh)
      shift
      if [[ $# -eq 0 ]]; then
        app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'var v = String((data.settings || {}).doh_server || ""); return v ? v : "(built-in: Google/Cloudflare/AliDNS)";'
        return 0
      fi
      value="$1"
      if [[ "$value" == "default" ]]; then
        value=""
      fi
      app_proxy_manifest_set_setting doh_server "$value" || return 1
      app_proxy_manifest_set_setting doh_prompted 1 || return 1
      echo "DoH server set to: ${value:-built-in (Google/Cloudflare/AliDNS)}"
      app_proxy_cli_regenerate
      ;;
    interface)
      shift
      if [[ $# -eq 0 ]]; then
        app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'var v = String((data.settings || {}).upstream_interface || ""); return v ? v : "(auto binding)";'
        return 0
      fi
      value="$1"
      if [[ "$value" == "auto" ]]; then
        value=""
      fi
      app_proxy_manifest_set_setting upstream_interface "$value" || return 1
      if [[ -n "$value" ]]; then
        app_proxy_manifest_set_setting upstream_interface_mode manual || return 1
      else
        app_proxy_manifest_set_setting upstream_interface_mode auto-binding || return 1
      fi
      echo "Upstream interface set to: ${value:-auto binding}"
      app_proxy_cli_regenerate
      ;;
    "")
      current_doh="$(app_proxy_cli_settings doh)"
      current_iface="$(app_proxy_cli_settings interface)"
      echo "Global settings (apply to the whole sing-box instance):"
      echo "  DoH server: $current_doh"
      echo "  Upstream interface: $current_iface"
      answer="$(app_proxy_cli_choose "Change:" $'DoH server\nUpstream interface\nBack')" || return 0
      case "$answer" in
        1)
          printf 'DoH server URL [Enter = built-in defaults]: ' >&2
          IFS= read -r value || value=""
          app_proxy_cli_settings doh "${value:-default}"
          ;;
        2)
          printf 'Interface name (e.g. en0) [Enter = auto binding]: ' >&2
          IFS= read -r value || value=""
          app_proxy_cli_settings interface "${value:-auto}"
          ;;
        *)
          ;;
      esac
      ;;
    *)
      echo "usage: app-proxy settings [doh <url|default> | interface <name|auto>]" >&2
      return 64
      ;;
  esac
}

app_proxy_cli_port_add() {
  local answer
  local profile_id
  local node_json
  local relay_id
  local nodes_array
  local port
  local port_input
  local manifest_snapshot
  local config_snapshot

  app_proxy_ensure_singbox_environment || return $?
  app_proxy_cli_ensure_manifest || return $?
  app_proxy_cli_maybe_prompt_doh || return 1
  app_proxy_cli_maybe_prompt_interface || return 1

  manifest_snapshot="$(app_proxy_cli_snapshot_file "$(app_proxy_manifest_path)")" || return 1
  config_snapshot="$(app_proxy_cli_snapshot_file "$(app_proxy_singbox_config_path)")" || {
    /bin/rm -f "$manifest_snapshot"
    return 1
  }

  answer="$(app_proxy_cli_choose "New port profile source:" \
    $'Subscription URL\nManual http/socks5 node\nLAN gateway first, subscription fallback (auto-switch by reachability)')" || return 1
  case "$answer" in
    3)
      {
        echo "--- Gateway-first profile ---"
        echo "Preferred egress: an HTTP/SOCKS proxy on your LAN (e.g. Surge on the"
        echo "gateway, http://192.168.x.x:6152). While it is reachable, all traffic"
        echo "of this port uses it. When this machine leaves that network, the"
        echo "profile automatically falls back to the subscription nodes you pick"
        echo "next, and switches back once the gateway is reachable again"
        echo "(health-checked every 5 minutes)."
      } >&2
      node_json="$(app_proxy_cli_prompt_manual_node)" || return 1
      profile_id="$(app_proxy_cli_add_subscription_profile_interactive "Fallback subscription URL: ")" || return 1
      app_proxy_manifest_update_profile "$profile_id" "{\"gateway_node\": $node_json}" || return 1
      ;;
    2)
      node_json="$(app_proxy_cli_prompt_manual_node)" || return 1
      relay_id="$(app_proxy_cli_prompt_relay)" || return 1
      if [[ -n "$relay_id" ]]; then
        node_json="$(app_proxy_cli_json_query "$node_json" "data.relay_profile = \"$relay_id\"; return JSON.stringify(data);")"
      fi
      node_json="$(app_proxy_cli_json_query "$node_json" 'data.selected = true; if (!data.name) { data.name = data.protocol + "-" + data.server; } return JSON.stringify(data);')"
      nodes_array="[$node_json]"

      port="$(app_proxy_cli_find_free_port)"
      printf 'Listen port [%s]: ' "$port"
      IFS= read -r port_input || port_input=""
      port="${port_input:-$port}"
      port="$(app_proxy_normalize_port "$port")" || {
        echo "invalid listen port" >&2
        return 1
      }
      profile_id="$(app_proxy_manifest_add_profile "{\"listen_port\": $port, \"source\": {\"kind\": \"manual\"}, \"nodes\": $nodes_array}")" || return 1
      ;;
    *)
      profile_id="$(app_proxy_cli_add_subscription_profile_interactive)" || return 1
      ;;
  esac

  port="$(app_proxy_json_get "$(app_proxy_manifest_get_profile "$profile_id")" listen_port)"
  if ! app_proxy_cli_apply_verified "$port" "$manifest_snapshot" "$config_snapshot"; then
    /bin/rm -f "$manifest_snapshot" "$config_snapshot"
    return 1
  fi
  /bin/rm -f "$manifest_snapshot" "$config_snapshot"
  echo "Profile $profile_id created."
  app_proxy_cli_post_add_binding_menu "$profile_id"
}

app_proxy_cli_bind_menu() {
  local profile_id

  app_proxy_cli_ensure_manifest || return $?
  profile_id="$(app_proxy_cli_prompt_profile "Bind to which profile?")" || return 1
  app_proxy_cli_post_add_binding_menu "$profile_id"
}

app_proxy_cli_node_labels() {
  app_proxy_cli_json_query "$1" 'return data.nodes.map(function(n){return (n.selected ? "[x] " : "[ ] ") + n.name + " (" + n.protocol + " " + n.server + ":" + n.server_port + ")" + (n.relay_profile ? "  [relay: " + n.relay_profile + "]" : "");}).join("\n");'
}

app_proxy_cli_pick_node() {
  # $1: profile json, $2: title; prints the 1-based node number on stdout
  local labels
  labels="$(app_proxy_cli_node_labels "$1")"
  app_proxy_cli_choose "$2" "$labels"
}

app_proxy_cli_apply_node_relay() {
  # $1: profile id, $2: 1-based node number, $3: relay profile id ("" = direct)
  local profile_id="$1"
  local node_number="$2"
  local relay_id="${3:-}"
  local profile_json
  local kind
  local patched

  profile_json="$(app_proxy_manifest_get_profile "$profile_id")" || return 1
  kind="$(app_proxy_cli_json_query "$profile_json" 'return data.source ? String(data.source.kind || "") : "";')"
  if [[ "$kind" != "manual" ]]; then
    echo "only manual profiles support per-node relay changes; subscription nodes follow their subscription" >&2
    return 1
  fi
  if [[ -n "$relay_id" ]]; then
    if [[ "$relay_id" == "$profile_id" ]]; then
      echo "a node cannot relay through its own profile" >&2
      return 1
    fi
    app_proxy_manifest_get_profile "$relay_id" >/dev/null || return 1
    if app_proxy_cli_relay_profile_incompatible "$relay_id"; then
      echo "profile $relay_id's egress uses XTLS Vision/REALITY, which cannot carry a relay (traffic would time out)." >&2
      echo "Pick a profile whose egress is anytls/trojan/ss/plain vless instead." >&2
      return 1
    fi
  fi

  patched="$(app_proxy_cli_json_query "$profile_json" "
    var index = Number(\"$node_number\") - 1;
    if (!data.nodes[index]) { throw new Error(\"invalid node number: $node_number\"); }
    if (\"$relay_id\") {
      data.nodes[index].relay_profile = \"$relay_id\";
    } else {
      delete data.nodes[index].relay_profile;
    }
    return JSON.stringify({nodes: data.nodes});
  ")" || return 1
  app_proxy_manifest_update_profile "$profile_id" "$patched" || return 1
  if [[ -n "$relay_id" ]]; then
    echo "Node now relays through $relay_id."
  else
    echo "Node now connects directly (no relay)."
  fi
  app_proxy_cli_regenerate
}

app_proxy_cli_node_menu() {
  local profile_id
  local profile_json
  local kind
  local actions
  local answer
  local action
  local pick
  local node_name
  local node_json
  local relay_id
  local patched
  local gateway_desc

  app_proxy_cli_ensure_manifest || return $?
  profile_id="$(app_proxy_cli_prompt_profile "Manage nodes of which profile?")" || return 1
  profile_json="$(app_proxy_manifest_get_profile "$profile_id")" || return 1
  kind="$(app_proxy_cli_json_query "$profile_json" 'return data.source ? String(data.source.kind || "") : "";')"
  gateway_desc="$(app_proxy_cli_json_query "$profile_json" 'var g = data.gateway_node; return g ? String(g.protocol) + " " + String(g.server) + ":" + String(g.server_port) : "";')"

  echo "Nodes of $profile_id ([x] = in the egress group; [relay: pfN] = routed through that profile):"
  if [[ -n "$gateway_desc" ]]; then
    echo "  Gateway egress: $gateway_desc  (preferred while reachable; nodes below are the fallback)"
  fi
  app_proxy_cli_node_labels "$profile_json" | /usr/bin/sed 's/^/  /'

  # actions depend on what this profile supports
  case "$kind" in
    manual)
      actions=$'Enable/disable a node (choose which nodes carry traffic)\nSet or change a node relay (route it through another configured line)\nEdit a node (re-enter server/port/credentials)\nDelete this profile (port)\nQuit'
      ;;
    subscription)
      actions=$'Enable/disable a node (choose which nodes carry traffic)\nUpdate from subscription (refresh the node list)\nGateway egress (set/replace/remove the preferred LAN gateway)\nDelete this profile (port)\nQuit'
      ;;
    *)
      actions=$'Enable/disable a node (choose which nodes carry traffic)\nDelete this profile (port)\nQuit'
      ;;
  esac
  answer="$(app_proxy_cli_choose "Actions for $profile_id:" "$actions")" || return 1
  action="$(printf '%s\n' "$actions" | /usr/bin/sed -n "${answer}p")"

  case "$action" in
    "Enable/disable a node"*)
      pick="$(app_proxy_cli_pick_node "$profile_json" "Which node? ([x] = currently enabled; picking it disables it, and vice versa)")" || return 1
      patched="$(app_proxy_cli_json_query "$profile_json" "
        var index = Number(\"$pick\") - 1;
        if (!data.nodes[index]) { throw new Error(\"invalid node number\"); }
        data.nodes[index].selected = !data.nodes[index].selected;
        if (!data.nodes.some(function(n){return n.selected;})) { throw new Error(\"at least one node must stay selected\"); }
        return JSON.stringify({nodes: data.nodes});
      ")" || return 1
      app_proxy_manifest_update_profile "$profile_id" "$patched" || return 1
      app_proxy_cli_regenerate
      ;;
    "Set or change a node relay"*)
      pick="$(app_proxy_cli_pick_node "$profile_json" "Change the relay of which node?")" || return 1
      relay_id="$(app_proxy_cli_prompt_relay)" || return 1
      app_proxy_cli_apply_node_relay "$profile_id" "$pick" "$relay_id"
      ;;
    "Edit a node"*)
      pick="$(app_proxy_cli_pick_node "$profile_json" "Edit which node? (you will re-enter all of its fields)")" || return 1
      node_name="$(app_proxy_cli_json_query "$profile_json" "var n = data.nodes[Number(\"$pick\")-1]; return n ? String(n.name) : \"\";")"
      if [[ -z "$node_name" ]]; then
        echo "invalid node number" >&2
        return 1
      fi
      node_json="$(app_proxy_cli_prompt_manual_node)" || return 1
      relay_id="$(app_proxy_cli_prompt_relay)" || return 1
      if [[ -n "$relay_id" ]]; then
        node_json="$(app_proxy_cli_json_query "$node_json" "data.relay_profile = \"$relay_id\"; return JSON.stringify(data);")"
      fi
      patched="$(app_proxy_cli_json_query "$profile_json" "
        var replacement = $node_json;
        replacement.selected = true;
        replacement.name = replacement.name || \"$node_name\";
        data.nodes[Number(\"$pick\")-1] = replacement;
        return JSON.stringify({nodes: data.nodes});
      ")" || return 1
      app_proxy_manifest_update_profile "$profile_id" "$patched" || return 1
      app_proxy_cli_regenerate
      ;;
    "Update from subscription"*)
      app_proxy_cli_update "$profile_id"
      ;;
    "Gateway egress"*)
      if [[ -n "$gateway_desc" ]]; then
        pick="$(app_proxy_cli_choose "Gateway egress is $gateway_desc:" \
          $'Replace it (enter a new gateway proxy)\nRemove it (always use the subscription nodes)\nQuit')" || return 1
      else
        echo "No gateway egress configured. When set, this port prefers the LAN gateway"
        echo "proxy while it is reachable and falls back to the subscription nodes"
        echo "automatically when it is not (e.g. away from home)."
        pick="$(app_proxy_cli_choose "Gateway egress:" $'Set one\nQuit')" || return 1
        [[ "$pick" == "1" ]] || return 0
      fi
      case "$pick" in
        1)
          node_json="$(app_proxy_cli_prompt_manual_node)" || return 1
          app_proxy_manifest_update_profile "$profile_id" "{\"gateway_node\": $node_json}" || return 1
          ;;
        2)
          app_proxy_manifest_update_profile "$profile_id" '{"gateway_node": null}' || return 1
          ;;
        *)
          return 0
          ;;
      esac
      app_proxy_cli_regenerate
      ;;
    "Delete this profile"*)
      app_proxy_cli_delete_port "$profile_id"
      ;;
    *)
      ;;
  esac
}

app_proxy_cli_clone_add() {
  # $1 (optional): target .app path; prompted for when omitted
  local target_app="${1:-}"
  local clone_name
  local copy_flag=""
  local answer
  local profile_id=""
  local port=""

  if [[ -z "$target_app" ]]; then
    echo "Drag the target .app here (Chromium/Electron apps, e.g. Claude or Codex), then press Return:"
    if ! IFS= read -r target_app; then
      echo "no app path provided" >&2
      return 1
    fi
  fi
  target_app="$(app_proxy_trim_dragged_path "$target_app")"
  if [[ -z "$target_app" ]]; then
    echo "app path is empty" >&2
    return 1
  fi

  printf 'Clone name (letters, digits, . _ -): '
  IFS= read -r clone_name || clone_name=""
  app_proxy_clone_validate_name "$clone_name" || return 1

  local clone_mode="shadow"
  answer="$(app_proxy_cli_choose "Clone type:" \
    $'Full clone (recommended): own identity, runs ALONGSIDE the original app\nLightweight: zero-copy launcher, shares the original app identity (Dock/TCC)')" || return 1
  if [[ "$answer" == "2" ]]; then
    clone_mode="light"
  fi

  if app_proxy_prompt_yes_no_default_no "Copy the main app's current data into the clone (default is a blank profile)?"; then
    copy_flag="copy"
  fi

  answer="$(app_proxy_cli_choose "Proxy for this clone?" \
    $'Direct connection (no proxy)\nBind to a port profile')" || return 1
  if [[ "$answer" == "2" ]]; then
    app_proxy_cli_ensure_manifest || return $?
    profile_id="$(app_proxy_cli_prompt_profile "Bind clone to which profile?")" || return 1
    port="$(app_proxy_json_get "$(app_proxy_manifest_get_profile "$profile_id")" listen_port)"
  fi

  app_proxy_generate_clone "$target_app" "$clone_name" "$port" "$copy_flag" "$clone_mode" || return $?
  echo "Launch it like a normal app: open \"$(/usr/bin/dirname "$target_app")/$(app_proxy_display_name "$target_app") · $clone_name.app\""
}

app_proxy_cli_pick_clone() {
  # $1 title; prints the selected clone name on stdout, prompts on stderr
  local title="$1"
  local list
  local labels
  local index
  local name

  list="$(app_proxy_clone_list)"
  if [[ -z "$list" ]]; then
    echo "no clones exist yet" >&2
    return 1
  fi
  labels="$(printf '%s\n' "$list" | /usr/bin/awk -F'\t' '{
    kind = ($3 == "shadow") ? "full" : $3
    printf "%s  [%s]  port %s  (%s)\n", $1, kind, $4, $2
  }')"
  index="$(app_proxy_cli_choose "$title" "$labels")" || return 1
  name="$(printf '%s\n' "$list" | /usr/bin/sed -n "${index}p" | /usr/bin/cut -f1)"
  if [[ -z "$name" ]]; then
    echo "invalid selection" >&2
    return 1
  fi
  printf '%s\n' "$name"
}

app_proxy_cli_clone_menu() {
  local sub="${1:-}"

  case "$sub" in
    add)
      shift
      app_proxy_cli_clone_add "$@"
      ;;
    list)
      shift
      local clones
      clones="$(app_proxy_clone_list)"
      if [[ -n "$clones" ]]; then
        printf '%s\n' "$clones" | while IFS=$'\t' read -r clone_name display mode port target clone_home; do
          local kind="$mode"
          [[ "$mode" == "shadow" ]] && kind="full"
          echo "$clone_name  [$kind]  ($display)  port: $port  target: $target"
          echo "  data: $clone_home"
        done
      else
        echo "(no clones)"
      fi
      ;;
    rename)
      shift
      if [[ -z "${1:-}" || -z "${2:-}" ]]; then
        echo "usage: app-proxy clone rename <name> <new-name>" >&2
        return 64
      fi
      app_proxy_clone_rename "$1" "$2"
      ;;
    delete)
      shift
      if [[ -z "${1:-}" ]]; then
        echo "usage: app-proxy clone delete <name> [--purge]" >&2
        return 64
      fi
      app_proxy_clone_delete "$@"
      ;;
    "")
      echo "Clones:"
      app_proxy_cli_clone_menu list
      echo
      local answer
      local name
      local new_name
      answer="$(app_proxy_cli_choose "Clone actions:" $'Add a clone\nRename a clone\nDelete a clone\nQuit')" || return 1
      case "$answer" in
        1)
          app_proxy_cli_clone_add
          ;;
        2)
          name="$(app_proxy_cli_pick_clone "Rename which clone?")" || return 1
          printf 'New name for %s: ' "$name" >&2
          IFS= read -r new_name || new_name=""
          app_proxy_clone_rename "$name" "$new_name"
          ;;
        3)
          name="$(app_proxy_cli_pick_clone "Delete which clone?")" || return 1
          if app_proxy_prompt_yes_no_default_no "Also delete $name's data home (logins/app data are lost)?"; then
            app_proxy_clone_delete "$name" --purge
          else
            app_proxy_clone_delete "$name"
          fi
          ;;
        *)
          ;;
      esac
      ;;
    *)
      echo "usage: app-proxy clone [add|list|rename|delete]" >&2
      return 64
      ;;
  esac
}

# --- manage apps (app-aggregated view: official app + wrappers + clones) ---------

app_proxy_cli_family_targets() {
  # unique official-app paths referenced by any wrapper/clone config
  local config
  local target

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    target="$(app_proxy_cli_wrapper_field "$config" TARGET_APP_PATH)"
    [[ -n "$target" ]] && printf '%s\n' "$target"
  done < <(app_proxy_cli_wrapper_config_paths) | /usr/bin/sort -u
}

app_proxy_cli_family_configs() {
  # $1: official app path -> its wrapper/clone config paths
  local target="$1"
  local config

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    if [[ "$(app_proxy_cli_wrapper_field "$config" TARGET_APP_PATH)" == "$target" ]]; then
      printf '%s\n' "$config"
    fi
  done < <(app_proxy_cli_wrapper_config_paths)
}

app_proxy_cli_instance_guard_plist() {
  # $1: config path -> guard LaunchAgent plist path (empty when unknown)
  local config="$1"
  local label
  local proxy_app

  label="$(app_proxy_cli_wrapper_field "$config" CLONE_GUARD_LABEL)"
  if [[ -z "$label" ]]; then
    proxy_app="$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)"
    if [[ -n "$proxy_app" && -f "$proxy_app/Contents/Info.plist" ]]; then
      label="$(app_proxy_plist_value "$proxy_app/Contents/Info.plist" CFBundleIdentifier).guard"
    fi
  fi
  if [[ -n "$label" ]]; then
    printf '%s/Library/LaunchAgents/%s.plist\n' "$(app_proxy_home)" "$label"
  fi
}

app_proxy_cli_instance_summary() {
  # $1: config path -> one human line describing the instance
  local config="$1"
  local name
  local clone_name
  local mode
  local port
  local clone_home
  local guard_plist
  local guard_state="no guard"
  local kind

  name="$(/usr/bin/basename "$(/usr/bin/dirname "$config")")"
  clone_name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
  mode="$(app_proxy_cli_wrapper_field "$config" CLONE_MODE)"
  port="$(app_proxy_cli_wrapper_field "$config" PROXY_PORT)"
  clone_home="$(app_proxy_cli_wrapper_field "$config" CLONE_HOME)"
  guard_plist="$(app_proxy_cli_instance_guard_plist "$config")"
  if [[ -n "$guard_plist" && -f "$guard_plist" ]]; then
    guard_state="guard"
  fi

  if [[ -n "$clone_name" ]]; then
    kind="clone '$clone_name'"
    [[ "$mode" == "shadow" ]] && kind="$kind [full]"
    [[ "$mode" == "light" ]] && kind="$kind [light]"
  else
    kind="proxy wrapper"
  fi
  printf '%s  (%s)  port %s  [%s]' "$name" "$kind" "${port:-direct}" "$guard_state"
  if [[ -n "$clone_home" ]]; then
    printf '  data: %s' "$clone_home"
  fi
  printf '\n'
}

app_proxy_cli_release_profile_port() {
  # after removing a binding/clone that used $1 (port): delete the now-unused
  # profile (ask, default delete); keep it with a notice when still referenced
  local port="$1"
  local profile_id
  local wrappers
  local deps_json
  local relay_count
  local profile_count

  [[ -n "$port" ]] || return 0
  profile_id="$(app_proxy_manifest_profile_for_port "$port" 2>/dev/null || true)"
  [[ -n "$profile_id" ]] || return 0

  wrappers="$(app_proxy_cli_wrapper_dependents "$port")"
  if [[ -n "$wrappers" ]]; then
    echo "Port $port (profile $profile_id) is still used by: $(printf '%s' "$wrappers" | /usr/bin/tr '\n' ' '); kept."
    return 0
  fi
  deps_json="$(app_proxy_manifest_profile_dependents "$profile_id" 2>/dev/null || true)"
  if [[ -n "$deps_json" ]]; then
    relay_count="$(app_proxy_cli_json_query "$deps_json" 'return String((data.relay_dependents || []).length);')"
    if [[ -n "$relay_count" && "$relay_count" != "0" ]]; then
      echo "Port $port (profile $profile_id) is still used as a relay by other profiles; kept."
      return 0
    fi
  fi
  profile_count="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return String(data.profiles.length);')"
  if [[ "$profile_count" -le 1 ]]; then
    echo "Profile $profile_id is the last one; kept (use the uninstaller to remove the environment)."
    return 0
  fi
  if app_proxy_prompt_yes_no_default_yes "Profile $profile_id (port $port) is no longer used by any app. Delete it?"; then
    app_proxy_cli_delete_port "$profile_id" --cascade || echo "Profile $profile_id could not be deleted; kept." >&2
  else
    echo "Kept profile $profile_id (port $port)."
  fi
}

app_proxy_cli_delete_instance_config() {
  # cascade-delete one instance (wrapper or clone): binding, guard, wrapper
  # package, (clone) data dir, then release its port profile via refcount
  local config="$1"
  local port
  local clone_name

  port="$(app_proxy_cli_wrapper_field "$config" PROXY_PORT)"
  clone_name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
  if [[ -n "$clone_name" ]]; then
    app_proxy_uninstall_clone_by_config "$config" || return 1
  else
    app_proxy_uninstall_proxy_app "$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)" || return 1
  fi
  app_proxy_cli_release_profile_port "$port"
}

app_proxy_cli_delete_family_nonofficial() {
  # $1: official app path; removes every proxy wrapper and clone of it,
  # keeping only the official app
  local target="$1"
  local config
  local status=0

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    app_proxy_cli_delete_instance_config "$config" || status=1
  done < <(app_proxy_cli_family_configs "$target")
  if [[ "$status" -eq 0 ]]; then
    echo "Only the official app remains: $target"
  fi
  return "$status"
}

app_proxy_cli_pick_family_config() {
  # $1: official app path, $2: title -> prints the chosen config path
  local target="$1"
  local title="$2"
  local configs
  local labels=""
  local config
  local pick

  configs="$(app_proxy_cli_family_configs "$target")"
  if [[ -z "$configs" ]]; then
    echo "no managed instances for this app" >&2
    return 1
  fi
  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    labels="$labels$(app_proxy_cli_instance_summary "$config")"$'\n'
  done <<<"$configs"
  pick="$(app_proxy_cli_choose "$title" "${labels%$'\n'}")" || return 1
  printf '%s\n' "$configs" | /usr/bin/sed -n "${pick}p"
}

app_proxy_cli_manage_family() {
  local target="$1"
  local config
  local answer
  local profile_id

  echo
  echo "App family: $(app_proxy_display_name "$target" 2>/dev/null || /usr/bin/basename "$target")"
  if [[ -d "$target" ]]; then
    echo "  official app: $target"
  else
    echo "  official app MISSING on disk: $target"
  fi
  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    echo "  $(app_proxy_cli_instance_summary "$config")"
  done < <(app_proxy_cli_family_configs "$target")

  answer="$(app_proxy_cli_choose "Actions:" \
    $'Clone this app (full/lightweight isolated instance)\nBind / rebind an instance to a port profile\nDelete an instance (cascade: binding, guard, wrapper package, clone data dir)\nDelete ALL non-official apps of this family\nBack')" || return 1
  case "$answer" in
    1)
      app_proxy_cli_clone_add "$target"
      ;;
    2)
      config="$(app_proxy_cli_pick_family_config "$target" "Bind which instance?")" || return 1
      profile_id="$(app_proxy_cli_prompt_profile "Bind to which profile?")" || return 1
      app_proxy_cli_bind_wrapper "$config" "$profile_id"
      ;;
    3)
      config="$(app_proxy_cli_pick_family_config "$target" "Delete which instance?")" || return 1
      app_proxy_cli_delete_instance_config "$config"
      ;;
    4)
      if app_proxy_prompt_yes_no_default_yes "Delete every proxy wrapper and clone of this app (the official app is kept)?"; then
        app_proxy_cli_delete_family_nonofficial "$target"
      else
        echo "Cancelled; nothing changed."
      fi
      ;;
    *)
      ;;
  esac
}

app_proxy_cli_manage_apps() {
  local targets
  local labels=""
  local target
  local family_count
  local pick
  local choices

  app_proxy_cli_ensure_manifest || return $?

  targets="$(app_proxy_cli_family_targets)"
  family_count=0
  if [[ -n "$targets" ]]; then
    while IFS= read -r target; do
      [[ -n "$target" ]] || continue
      family_count=$((family_count + 1))
      labels="$labels$(/usr/bin/basename "$target" .app)  ($(app_proxy_cli_family_configs "$target" | /usr/bin/grep -c .) managed instance(s))  $target"$'\n'
    done <<<"$targets"
  fi
  choices="${labels}Bind a NEW app (drag the .app, creates a proxy wrapper)
Create a clone of a NEW app
Back"

  pick="$(app_proxy_cli_choose "Manage apps:" "$choices")" || return 1
  if [[ "$family_count" -gt 0 && "$pick" -le "$family_count" ]]; then
    target="$(printf '%s\n' "$targets" | /usr/bin/sed -n "${pick}p")"
    app_proxy_cli_manage_family "$target"
    return $?
  fi
  case $((pick - family_count)) in
    1)
      app_proxy_cli_bind_menu
      ;;
    2)
      app_proxy_cli_clone_add
      ;;
    *)
      ;;
  esac
}

# --- doctor ----------------------------------------------------------------------

app_proxy_cli_doctor() {
  local overall=0
  local manifest_json
  local var
  local leaked=""
  local rows=""
  local id
  local port
  local relayed
  local exit_ip
  local dupes
  local config
  local wrapper_name
  local wrapper_port
  local manifest_ports

  app_proxy_cli_ensure_manifest || return $?
  manifest_json="$(app_proxy_manifest_read)" || return 1

  # 1. proxy environment leak: process-level env bypasses per-app bindings
  for var in HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; do
    if [[ -n "$(eval "printf '%s' \"\${$var:-}\"")" ]]; then
      leaked="$leaked$var "
    fi
  done
  if [[ -n "$leaked" ]]; then
    echo "[!] proxy environment detected in this shell: $leaked"
    echo "    These variables override per-app bindings for every process started here."
  fi

  # 2. port -> profile -> exit IP table
  echo "Port -> profile -> exit IP:"
  while IFS=$'\t' read -r id port relayed; do
    [[ -n "$id" ]] || continue
    if ! app_proxy_cmd nc -z 127.0.0.1 "$port" >/dev/null 2>&1; then
      echo "  $port  $id  NOT LISTENING  <- run: brew services restart sing-box (or app-proxy apply)"
      overall=1
      continue
    fi
    exit_ip="$(app_proxy_cmd curl -x "http://127.0.0.1:$port" -fsS --max-time 20 https://ifconfig.me 2>/dev/null || true)"
    exit_ip="${exit_ip//$'\n'/}"
    exit_ip="${exit_ip//$'\r'/}"
    if [[ -z "$exit_ip" ]]; then
      echo "  $port  $id  NO EGRESS (exit IP request failed)"
      overall=1
      continue
    fi
    echo "  $port  $id  $exit_ip$([[ "$relayed" == "1" ]] && printf ' (relay detour)')"
    if [[ "$relayed" != "1" ]]; then
      rows="$rows$exit_ip $id"$'\n'
    fi
  done < <(app_proxy_cli_json_query "$manifest_json" '
    return data.profiles.map(function (p) {
      var relayed = (p.nodes || []).some(function (n) { return n.selected && n.relay_profile; });
      return p.id + "\t" + p.listen_port + "\t" + (relayed ? "1" : "0");
    }).join("\n");
  ')

  # 3. duplicated exit IPs across non-relay profiles usually mean traffic is
  #    NOT going through the intended per-port egress
  dupes="$(printf '%s' "$rows" | /usr/bin/awk '{ips[$1] = ips[$1] ? ips[$1] ", " $2 : $2; count[$1]++} END {for (ip in count) if (count[ip] > 1) print ip ": " ips[ip]}')"
  if [[ -n "$dupes" ]]; then
    echo "[!] identical exit IP on multiple independent profiles (isolation may be broken):"
    printf '%s\n' "$dupes" | /usr/bin/sed 's/^/    /'
    overall=1
  fi

  # 4. config.env port desync against the manifest
  manifest_ports="$(app_proxy_cli_json_query "$manifest_json" 'return data.profiles.map(function(p){return String(p.listen_port);}).join(" ");')"
  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    wrapper_name="$(/usr/bin/basename "$(/usr/bin/dirname "$config")")"
    wrapper_port="$(app_proxy_cli_wrapper_field "$config" PROXY_PORT)"
    [[ -n "$wrapper_port" ]] || continue
    if [[ " $manifest_ports " != *" $wrapper_port "* ]]; then
      echo "[!] $wrapper_name binds port $wrapper_port, which no manifest profile provides (stale config.env)"
      overall=1
    fi
  done < <(app_proxy_cli_wrapper_config_paths)

  # 5. orphan guard background items: dead plists AND launchd jobs still loaded
  #    with no config file (these show in System Settings > App Background
  #    Activity). Offer to clean them right here (default: yes).
  if ! app_proxy_sweep_orphan_guards; then
    overall=1
  fi

  # 6. clones whose embedded runtime scripts predate the installed templates
  #    (they miss later launcher fixes, e.g. the "Keychain Not Found" bridge);
  #    offer to repair them in place (default: yes)
  if ! app_proxy_repair_stale_clone_runtimes; then
    overall=1
  fi

  if [[ "$overall" -eq 0 ]]; then
    echo "doctor: all checks passed"
  fi
  return "$overall"
}

app_proxy_cli_menu() {
  local answer
  local choose_status
  local warnings

  app_proxy_cli_ensure_manifest || return $?

  warnings="$(app_proxy_cli_json_query "$(app_proxy_manifest_read)" 'return (data.pending_warnings || []).map(function(w){return "[!] " + w.message;}).join("\n");')"
  if [[ -n "$warnings" ]]; then
    echo "Pending warnings:"
    printf '%s\n' "$warnings"
    if app_proxy_prompt_yes_no_default_no "Clear these warnings?"; then
      app_proxy_manifest_clear_warnings
    fi
    echo
  fi

  while true; do
    choose_status=0
    answer="$(app_proxy_cli_choose "app-proxy:" \
      $'Status\nAdd a port profile\nManage nodes\nUpdate subscriptions\nManage apps (bind / clone / delete instances)\nTest proxy connectivity (exit IP / DNS)\nDoctor (diagnose ports, exit IPs, guards, desyncs)\nGlobal settings (DoH / interface)\nQuit')" || choose_status=$?
    if [[ "$choose_status" -eq 1 ]]; then
      # invalid input: show the menu again
      continue
    fi
    if [[ "$choose_status" -ne 0 ]]; then
      # EOF or TUI cancel: leave quietly
      return 0
    fi
    case "$answer" in
      1) app_proxy_cli_status || echo "(operation failed; back to the menu)" >&2 ;;
      2) app_proxy_cli_port_add || echo "(operation failed; back to the menu)" >&2 ;;
      3) app_proxy_cli_node_menu || echo "(operation failed; back to the menu)" >&2 ;;
      4) app_proxy_cli_update || echo "(operation failed; back to the menu)" >&2 ;;
      5) app_proxy_cli_manage_apps || echo "(operation failed; back to the menu)" >&2 ;;
      6) app_proxy_cli_test || echo "(operation failed; back to the menu)" >&2 ;;
      7) app_proxy_cli_doctor || echo "(operation failed; back to the menu)" >&2 ;;
      8) app_proxy_cli_settings || echo "(operation failed; back to the menu)" >&2 ;;
      *) return 0 ;;
    esac
    echo
  done
}

app_proxy_cli_usage() {
  echo "usage: app-proxy [command]"
  echo
  echo "commands:"
  echo "  (none)            interactive menu"
  echo "  status            list profiles, Claude rules target, wrapper bindings, clones"
  echo "  port add          add a port profile (subscription, manual node, or LAN"
  echo "                    gateway with subscription fallback)"
  echo "  port delete <id> [--cascade|--cascade-delete-dependents]"
  echo "                    delete a port profile. --cascade removes bound wrappers/"
  echo "                    clones and converts relay dependents to direct connection;"
  echo "                    --cascade-delete-dependents deletes the relay-dependent"
  echo "                    profiles too, including their bound apps/wrappers/guards"
  echo "  node              manage nodes of a profile (toggle/edit/update/delete)"
  echo "  update [id|--all] [--non-interactive]  refresh subscriptions"
  echo "  test [id...]      per-port health check: inbound, exit IP, DNS, connectivity"
  echo "  doctor            full diagnosis: port->profile->exit-IP table, dead ports,"
  echo "                    duplicated exit IPs, orphan guards, config.env/manifest"
  echo "                    desyncs, proxy env leaks in the current shell, and"
  echo "                    clones with outdated embedded runtime scripts (repair)"
  echo "  settings [doh <url|default>] [interface <name|auto>]  global sing-box settings"
  echo "  apply             regenerate the sing-box config from the manifest and restart"
  echo "  apps              manage apps: official app + wrappers + clones per family,"
  echo "                    bind/clone/delete with cascade and profile refcounting"
  echo "  bind              create or rebind an app proxy wrapper"
  echo "  clone add         create an isolated app instance (own HOME/data, optional proxy)"
  echo "  clone list        list clones"
  echo "  clone rename <name> <new-name>"
  echo "  clone delete <name> [--purge]   delete a clone (data kept unless --purge)"
  echo "  uninstall         run the uninstaller (environment / wrappers / clones)"
  echo
  echo "Every command accepts -h/--help."
}

app_proxy_cli_main() {
  local command="${1:-}"

  if [[ -n "$command" ]]; then
    shift
    if app_proxy_cli_help_requested "$@"; then
      app_proxy_cli_usage
      return 0
    fi
    set -- "$command" "$@"
  fi

  case "$command" in
    status)
      shift
      app_proxy_cli_ensure_manifest || return $?
      app_proxy_cli_status "$@"
      ;;
    update)
      shift
      app_proxy_cli_ensure_manifest || return $?
      app_proxy_cli_update "$@"
      ;;
    test)
      shift
      app_proxy_cli_ensure_manifest || return $?
      app_proxy_cli_test "$@"
      ;;
    doctor)
      shift
      app_proxy_cli_doctor "$@"
      ;;
    settings)
      shift
      app_proxy_cli_settings "$@"
      ;;
    apply)
      shift
      app_proxy_cli_ensure_manifest || return $?
      app_proxy_cli_regenerate
      ;;
    bind)
      shift
      app_proxy_cli_bind_menu "$@"
      ;;
    apps)
      shift
      app_proxy_cli_manage_apps "$@"
      ;;
    node)
      shift
      app_proxy_cli_node_menu "$@"
      ;;
    clone)
      shift
      app_proxy_cli_clone_menu "$@"
      ;;
    uninstall)
      shift
      app_proxy_uninstall_main "$@"
      ;;
    port)
      shift
      case "${1:-}" in
        add)
          shift
          app_proxy_cli_port_add "$@"
          ;;
        delete)
          shift
          if [[ -z "${1:-}" ]]; then
            echo "usage: app-proxy port delete <profile-id> [--cascade]" >&2
            return 64
          fi
          app_proxy_cli_ensure_manifest || return $?
          app_proxy_cli_delete_port "$@"
          ;;
        *)
          app_proxy_cli_usage >&2
          return 64
          ;;
      esac
      ;;
    ""|menu)
      app_proxy_cli_menu
      ;;
    help|-h|--help)
      app_proxy_cli_usage
      ;;
    *)
      app_proxy_cli_usage >&2
      return 64
      ;;
  esac
}
