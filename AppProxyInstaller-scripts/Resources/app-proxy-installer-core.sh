#!/bin/bash

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "This V2 installer core is meant to be sourced by install-app-proxy.command." >&2
  exit 64
fi

APP_PROXY_INSTALLER_CORE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-common.sh"
. "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-singbox.sh"
. "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-manifest.sh"

app_proxy_home() {
  printf '%s\n' "${APP_PROXY_HOME:-$HOME}"
}

app_proxy_xml_escape() {
  printf '%s' "$1" \
    | /usr/bin/sed \
      -e 's/&/\&amp;/g' \
      -e 's/</\&lt;/g' \
      -e 's/>/\&gt;/g' \
      -e 's/"/\&quot;/g' \
      -e "s/'/\&apos;/g"
}

app_proxy_safe_path_component() {
  local component="$1"

  if [[ -z "$component" ]]; then
    echo "display name is empty and cannot be used as a path component" >&2
    return 1
  fi
  if [[ "$component" == *"/"* || "$component" == *":"* ]]; then
    echo "display name contains a character that cannot be used as a path component: $component" >&2
    return 1
  fi
  if /usr/bin/printf '%s' "$component" | /usr/bin/grep -q '[[:cntrl:]]'; then
    echo "display name contains a control character and cannot be used as a path component" >&2
    return 1
  fi

  printf '%s\n' "$component"
}

app_proxy_compare_path() {
  local path="$1"
  local parent
  local name

  parent="$(/usr/bin/dirname "$path")"
  name="$(/usr/bin/basename "$path")"
  if [[ -d "$parent" ]]; then
    parent="$(cd "$parent" && pwd -P)"
  fi
  printf '%s/%s\n' "$parent" "$name"
}

app_proxy_write_proxy_info_plist() {
  local plist="$1"
  local app_name="$2"
  local bundle_id="$3"
  local icon_file="$4"
  local icon_name="${5:-}"

  /bin/cat >"$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>$(app_proxy_xml_escape "$app_name")</string>
  <key>CFBundleDisplayName</key>
  <string>$(app_proxy_xml_escape "$app_name")</string>
  <key>CFBundleIdentifier</key>
  <string>$(app_proxy_xml_escape "$bundle_id")</string>
  <key>CFBundleExecutable</key>
  <string>app-proxy-launcher</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
PLIST
  if [[ -n "$icon_file" ]]; then
    /bin/cat >>"$plist" <<PLIST
  <key>CFBundleIconFile</key>
  <string>$(app_proxy_xml_escape "$icon_file")</string>
PLIST
  fi
  if [[ -n "$icon_name" ]]; then
    /bin/cat >>"$plist" <<PLIST
  <key>CFBundleIconName</key>
  <string>$(app_proxy_xml_escape "$icon_name")</string>
PLIST
  fi
  /bin/cat >>"$plist" <<'PLIST'
</dict>
</plist>
PLIST
}

app_proxy_write_guard_launch_agent() {
  local plist="$1"
  local label="$2"
  local guard_path="$3"
  local display_name="${4:-}"
  local assoc_bundle="${label%.guard}"
  local named_guard

  # System Settings shows a launchd item under the program's file name, and BTM
  # attribution via AssociatedBundleIdentifiers is unreliable for ad-hoc signed
  # bundles -- so give each guard a per-app-named symlink and point the job at
  # it; identical "app-proxy-guard" rows become "<App> Guard" rows.
  if [[ -n "$display_name" ]]; then
    named_guard="$(/usr/bin/dirname "$guard_path")/$display_name Guard"
    if /bin/ln -sfh "$(/usr/bin/basename "$guard_path")" "$named_guard" 2>/dev/null; then
      guard_path="$named_guard"
    fi
  fi

  # AssociatedBundleIdentifiers ties the guard to its proxy/clone app bundle so
  # System Settings > Login Items > App Background Activity lists it under that
  # app's name and icon, instead of a wall of identical "app-proxy-guard" rows.
  /bin/cat >"$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$(app_proxy_xml_escape "$label")</string>
  <key>AssociatedBundleIdentifiers</key>
  <array>
    <string>$(app_proxy_xml_escape "$assoc_bundle")</string>
  </array>
  <key>ProgramArguments</key>
  <array>
    <string>$(app_proxy_xml_escape "$guard_path")</string>
  </array>
  <key>KeepAlive</key>
  <false/>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
PLIST
}

app_proxy_proxy_url() {
  local proxy_host="$1"
  local proxy_port="$2"

  printf 'http://%s:%s\n' "$proxy_host" "$proxy_port"
}

app_proxy_prompt_yes_no_default_yes() {
  local prompt="$1"
  local answer

  if [[ "${APP_PROXY_NONINTERACTIVE:-0}" == "1" ]]; then
    return 0
  fi

  printf '%s [Y/n] ' "$prompt"
  IFS= read -r answer || answer=""
  case "$answer" in
    n|N)
      return 1
      ;;
  esac
  return 0
}

app_proxy_prompt_yes_no_default_no() {
  local prompt="$1"
  local answer

  if [[ "${APP_PROXY_NONINTERACTIVE:-0}" == "1" ]]; then
    return 1
  fi

  printf '%s [y/N] ' "$prompt"
  IFS= read -r answer || answer=""
  case "$answer" in
    y|Y)
      return 0
      ;;
  esac
  return 1
}

app_proxy_backup_file_if_exists() {
  local path="$1"
  local backup

  if [[ ! -f "$path" ]]; then
    return 0
  fi
  backup="$path.bak-app-proxy-$(/bin/date '+%Y%m%d-%H%M%S')"
  /bin/cp "$path" "$backup" || return 1
  echo "  Backup created: $backup"
}

app_proxy_target_kind() {
  local target_app="$1"
  local display_name="$2"
  local bundle_id="$3"
  local identity

  identity="$(printf '%s %s %s' "$target_app" "$display_name" "$bundle_id" | /usr/bin/tr '[:upper:]' '[:lower:]')"
  if [[ "$identity" == *"claude"* || "$identity" == *"anthropic"* ]]; then
    printf '%s\n' "claude"
    return 0
  fi
  if [[ "$identity" == *"codex"* || "$identity" == *"openai"* ]]; then
    printf '%s\n' "codex"
    return 0
  fi
  printf '\n'
}

app_proxy_detect_target_kind() {
  local target_app="$1"
  local target_plist="$target_app/Contents/Info.plist"
  local display_name
  local bundle_id

  if [[ ! -f "$target_plist" ]]; then
    printf '\n'
    return 0
  fi

  display_name="$(app_proxy_display_name "$target_app")"
  bundle_id="$(app_proxy_plist_value "$target_plist" CFBundleIdentifier)"
  app_proxy_target_kind "$target_app" "$display_name" "$bundle_id"
}

app_proxy_report_no_managed_proxy_auto() {
  echo "  Current sing-box config does not contain the installer-managed proxy-auto outbound." >&2
  echo "  Claude domain rules are not allowed to use route.final, direct, or an arbitrary existing outbound." >&2
  echo "  Re-run the installer without an existing usable sing-box proxy, or replace the sing-box config through this installer so Claude rules can use the selected subscription nodes." >&2
}

app_proxy_json_env_status() {
  local path="$1"
  local proxy_url="$2"

  if [[ ! -f "$path" ]]; then
    printf '%s\n' "missing"
    return 0
  fi

  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function run(argv) {
  var path = argv[0];
  var proxyUrl = argv[1];
  var keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
  var fm = $.NSFileManager.defaultManager;
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  var config;
  var env;
  var found = 0;
  var conflicts = 0;
  if (!fm.fileExistsAtPath(path)) return "missing";
  if (!text) return "invalid";
  try {
    config = JSON.parse(ObjC.unwrap(text));
  } catch (error) {
    return "invalid";
  }
  env = config && typeof config.env === "object" && !Array.isArray(config.env) ? config.env : {};
  keys.forEach(function(key) {
    if (Object.prototype.hasOwnProperty.call(env, key)) {
      found += 1;
      if (String(env[key]) !== proxyUrl) conflicts += 1;
    }
  });
  if (found === 0) return "not-configured";
  if (conflicts > 0 || found < keys.length) return "conflict";
  return "configured";
}
' "$path" "$proxy_url"
}

app_proxy_json_env_set_proxy() {
  local path="$1"
  local proxy_url="$2"

  /bin/mkdir -p "$(/usr/bin/dirname "$path")" || return 1
  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function readConfig(path) {
  var fm = $.NSFileManager.defaultManager;
  if (!fm.fileExistsAtPath(path)) return {};
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  if (!text) return {};
  return JSON.parse(ObjC.unwrap(text));
}

function writeConfig(path, config) {
  var text = JSON.stringify(config, null, 2) + "\n";
  var nsText = $.NSString.alloc.initWithUTF8String(text);
  nsText.writeToFileAtomicallyEncodingError(path, true, $.NSUTF8StringEncoding, null);
}

function run(argv) {
  var path = argv[0];
  var proxyUrl = argv[1];
  var keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
  var config = readConfig(path);
  if (!config || typeof config !== "object" || Array.isArray(config)) {
    throw new Error("settings root must be an object");
  }
  if (!config.env || typeof config.env !== "object" || Array.isArray(config.env)) {
    config.env = {};
  }
  keys.forEach(function(key) {
    config.env[key] = proxyUrl;
  });
  writeConfig(path, config);
}
' "$path" "$proxy_url"
}

app_proxy_json_env_remove_proxy() {
  local path="$1"
  local proxy_url="$2"

  [[ -f "$path" ]] || return 0
  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function run(argv) {
  var path = argv[0];
  var proxyUrl = argv[1];
  var keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
  var fm = $.NSFileManager.defaultManager;
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  var config;
  var changed = false;
  if (!fm.fileExistsAtPath(path) || !text) return "";
  config = JSON.parse(ObjC.unwrap(text));
  if (!config.env || typeof config.env !== "object" || Array.isArray(config.env)) return "";
  keys.forEach(function(key) {
    if (Object.prototype.hasOwnProperty.call(config.env, key) && String(config.env[key]) === proxyUrl) {
      delete config.env[key];
      changed = true;
    }
  });
  if (changed) {
    var nsText = $.NSString.alloc.initWithUTF8String(JSON.stringify(config, null, 2) + "\n");
    nsText.writeToFileAtomicallyEncodingError(path, true, $.NSUTF8StringEncoding, null);
  }
}
' "$path" "$proxy_url"
}

app_proxy_json_env_remove_local_proxy() {
  local path="$1"

  [[ -f "$path" ]] || return 0
  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function isLocalProxy(value) {
  return /^https?:\/\/127\.0\.0\.1:[0-9]+$/.test(String(value)) || /^127\.0\.0\.1:[0-9]+$/.test(String(value));
}

function run(argv) {
  var path = argv[0];
  var keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
  var fm = $.NSFileManager.defaultManager;
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  var config;
  var changed = false;
  if (!fm.fileExistsAtPath(path) || !text) return "";
  config = JSON.parse(ObjC.unwrap(text));
  if (!config.env || typeof config.env !== "object" || Array.isArray(config.env)) return "";
  keys.forEach(function(key) {
    if (Object.prototype.hasOwnProperty.call(config.env, key) && isLocalProxy(config.env[key])) {
      delete config.env[key];
      changed = true;
    }
  });
  if (changed) {
    var nsText = $.NSString.alloc.initWithUTF8String(JSON.stringify(config, null, 2) + "\n");
    nsText.writeToFileAtomicallyEncodingError(path, true, $.NSUTF8StringEncoding, null);
  }
}
' "$path"
}

app_proxy_env_file_status() {
  local path="$1"
  local proxy_url="$2"
  local result

  if [[ ! -f "$path" ]]; then
    printf '%s\n' "missing"
    return 0
  fi
  result="$(/usr/bin/awk -F= -v proxy="$proxy_url" '
    BEGIN {
      split("HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy", keys, " ");
      for (i in keys) wanted[keys[i]] = 1;
    }
    $1 in wanted {
      found += 1;
      value = $0;
      sub(/^[^=]*=/, "", value);
      gsub(/^'\''|'\''$/, "", value);
      gsub(/^"|"$/, "", value);
      if (value != proxy) conflicts += 1;
    }
    END {
      if (found == 0) print "not-configured";
      else if (conflicts > 0 || found < 6) print "conflict";
      else print "configured";
    }
  ' "$path")"
  printf '%s\n' "$result"
}

app_proxy_env_file_set_proxy() {
  local path="$1"
  local proxy_url="$2"
  local temp_file

  /bin/mkdir -p "$(/usr/bin/dirname "$path")" || return 1
  temp_file="$(/usr/bin/mktemp "${TMPDIR:-/tmp}/app-proxy-env.XXXXXX")" || return 1
  if [[ -f "$path" ]]; then
    /usr/bin/awk -F= '
      BEGIN {
        split("HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy", keys, " ");
        for (i in keys) skip[keys[i]] = 1;
      }
      !($1 in skip) { print $0 }
    ' "$path" >"$temp_file"
  fi
  {
    printf 'HTTP_PROXY=%s\n' "$proxy_url"
    printf 'HTTPS_PROXY=%s\n' "$proxy_url"
    printf 'ALL_PROXY=%s\n' "$proxy_url"
    printf 'http_proxy=%s\n' "$proxy_url"
    printf 'https_proxy=%s\n' "$proxy_url"
    printf 'all_proxy=%s\n' "$proxy_url"
  } >>"$temp_file"
  /bin/mv "$temp_file" "$path"
}

app_proxy_env_file_remove_proxy() {
  local path="$1"
  local proxy_url="$2"
  local temp_file

  [[ -f "$path" ]] || return 0
  temp_file="$(/usr/bin/mktemp "${TMPDIR:-/tmp}/app-proxy-env.XXXXXX")" || return 1
  /usr/bin/awk -F= -v proxy="$proxy_url" '
    BEGIN {
      split("HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy", keys, " ");
      for (i in keys) maybe[keys[i]] = 1;
    }
    {
      value = $0;
      sub(/^[^=]*=/, "", value);
      clean = value;
      gsub(/^'\''|'\''$/, "", clean);
      gsub(/^"|"$/, "", clean);
      if (($1 in maybe) && clean == proxy) next;
      print $0;
    }
  ' "$path" >"$temp_file"
  /bin/mv "$temp_file" "$path"
}

app_proxy_env_file_remove_local_proxy() {
  local path="$1"
  local temp_file

  [[ -f "$path" ]] || return 0
  temp_file="$(/usr/bin/mktemp "${TMPDIR:-/tmp}/app-proxy-env.XXXXXX")" || return 1
  /usr/bin/awk -F= '
    BEGIN {
      split("HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy", keys, " ");
      for (i in keys) maybe[keys[i]] = 1;
    }
    {
      value = $0;
      sub(/^[^=]*=/, "", value);
      clean = value;
      gsub(/^'\''|'\''$/, "", clean);
      gsub(/^"|"$/, "", clean);
      if (($1 in maybe) && (clean ~ /^https?:\/\/127\.0\.0\.1:[0-9]+$/ || clean ~ /^127\.0\.0\.1:[0-9]+$/)) next;
      print $0;
    }
  ' "$path" >"$temp_file"
  /bin/mv "$temp_file" "$path"
}

app_proxy_toml_status() {
  local path="$1"
  local proxy_url="$2"

  if [[ ! -f "$path" ]]; then
    printf '%s\n' "missing"
    return 0
  fi
  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function run(argv) {
  var path = argv[0];
  var proxyUrl = argv[1];
  var keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
  var fm = $.NSFileManager.defaultManager;
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  var lines;
  var inTable = false;
  var found = 0;
  var conflicts = 0;
  if (!fm.fileExistsAtPath(path)) return "missing";
  if (!text) return "missing";
  lines = ObjC.unwrap(text).split(/\r?\n/);
  lines.forEach(function(line) {
    var table = line.match(/^\s*\[([^\]]+)\]\s*$/);
    var match;
    if (table) {
      inTable = table[1] === "shell_environment_policy.set";
      return;
    }
    if (!inTable) return;
    match = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"(.*)"\s*$/);
    if (!match) return;
    if (keys.indexOf(match[1]) !== -1) {
      found += 1;
      if (match[2] !== proxyUrl) conflicts += 1;
    }
  });
  if (found === 0) return "not-configured";
  if (conflicts > 0 || found < keys.length) return "conflict";
  return "configured";
}
' "$path" "$proxy_url"
}

app_proxy_toml_set_proxy() {
  local path="$1"
  local proxy_url="$2"

  /bin/mkdir -p "$(/usr/bin/dirname "$path")" || return 1
  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function proxyLines(proxyUrl) {
  return [
    "HTTP_PROXY = \"" + proxyUrl + "\"",
    "HTTPS_PROXY = \"" + proxyUrl + "\"",
    "ALL_PROXY = \"" + proxyUrl + "\"",
    "http_proxy = \"" + proxyUrl + "\"",
    "https_proxy = \"" + proxyUrl + "\"",
    "all_proxy = \"" + proxyUrl + "\""
  ];
}

function run(argv) {
  var path = argv[0];
  var proxyUrl = argv[1];
  var fm = $.NSFileManager.defaultManager;
  var keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  var lines = fm.fileExistsAtPath(path) && text ? ObjC.unwrap(text).split(/\r?\n/) : [];
  var out = [];
  var inTable = false;
  var foundTable = false;
  var inserted = false;
  function isProxyLine(line) {
    var match = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=/);
    return match && keys.indexOf(match[1]) !== -1;
  }
  function insertProxyLines() {
    if (!inserted) {
      proxyLines(proxyUrl).forEach(function(line) { out.push(line); });
      inserted = true;
    }
  }
  lines.forEach(function(line) {
    var table = line.match(/^\s*\[([^\]]+)\]\s*$/);
    if (table) {
      if (inTable) insertProxyLines();
      inTable = table[1] === "shell_environment_policy.set";
      if (inTable) foundTable = true;
      out.push(line);
      return;
    }
    if (inTable && isProxyLine(line)) return;
    out.push(line);
  });
  if (foundTable) {
    if (inTable) insertProxyLines();
  } else {
    if (out.length && out[out.length - 1] !== "") out.push("");
    out.push("[shell_environment_policy.set]");
    proxyLines(proxyUrl).forEach(function(line) { out.push(line); });
  }
  while (out.length > 1 && out[out.length - 1] === "" && out[out.length - 2] === "") {
    out.pop();
  }
  var nsText = $.NSString.alloc.initWithUTF8String(out.join("\n").replace(/\n*$/, "") + "\n");
  nsText.writeToFileAtomicallyEncodingError(path, true, $.NSUTF8StringEncoding, null);
}
' "$path" "$proxy_url"
}

app_proxy_toml_remove_proxy() {
  local path="$1"
  local proxy_url="$2"

  [[ -f "$path" ]] || return 0
  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function run(argv) {
  var path = argv[0];
  var proxyUrl = argv[1];
  var keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
  var fm = $.NSFileManager.defaultManager;
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  var lines;
  var out = [];
  var inTable = false;
  if (!fm.fileExistsAtPath(path) || !text) return "";
  lines = ObjC.unwrap(text).split(/\r?\n/);
  lines.forEach(function(line) {
    var table = line.match(/^\s*\[([^\]]+)\]\s*$/);
    var match;
    if (table) {
      inTable = table[1] === "shell_environment_policy.set";
      out.push(line);
      return;
    }
    if (inTable) {
      match = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"(.*)"\s*$/);
      if (match && keys.indexOf(match[1]) !== -1 && match[2] === proxyUrl) {
        return;
      }
    }
    out.push(line);
  });
  var nsText = $.NSString.alloc.initWithUTF8String(out.join("\n").replace(/\n*$/, "") + "\n");
  nsText.writeToFileAtomicallyEncodingError(path, true, $.NSUTF8StringEncoding, null);
}
' "$path" "$proxy_url"
}

app_proxy_toml_remove_local_proxy() {
  local path="$1"

  [[ -f "$path" ]] || return 0
  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function isLocalProxy(value) {
  return /^https?:\/\/127\.0\.0\.1:[0-9]+$/.test(String(value)) || /^127\.0\.0\.1:[0-9]+$/.test(String(value));
}

function run(argv) {
  var path = argv[0];
  var keys = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
  var fm = $.NSFileManager.defaultManager;
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  var lines;
  var out = [];
  var inTable = false;
  if (!fm.fileExistsAtPath(path) || !text) return "";
  lines = ObjC.unwrap(text).split(/\r?\n/);
  lines.forEach(function(line) {
    var table = line.match(/^\s*\[([^\]]+)\]\s*$/);
    var match;
    if (table) {
      inTable = table[1] === "shell_environment_policy.set";
      out.push(line);
      return;
    }
    if (inTable) {
      match = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"(.*)"\s*$/);
      if (match && keys.indexOf(match[1]) !== -1 && isLocalProxy(match[2])) {
        return;
      }
    }
    out.push(line);
  });
  var nsText = $.NSString.alloc.initWithUTF8String(out.join("\n").replace(/\n*$/, "") + "\n");
  nsText.writeToFileAtomicallyEncodingError(path, true, $.NSUTF8StringEncoding, null);
}
' "$path"
}

app_proxy_configure_json_env_proxy() {
  local label="$1"
  local path="$2"
  local proxy_url="$3"
  local status

  status="$(app_proxy_json_env_status "$path" "$proxy_url")"
  case "$status" in
    missing)
      if ! app_proxy_prompt_yes_no_default_yes "$label settings file not found. Create it at $path?"; then
        echo "$label proxy config was not created; installation stopped." >&2
        return 76
      fi
      ;;
    invalid)
      echo "$label settings file is not valid JSON and cannot be updated: $path" >&2
      return 76
      ;;
    conflict)
      if ! app_proxy_prompt_yes_no_default_yes "$label settings already contain proxy variables. Overwrite them with $proxy_url?"; then
        echo "$label proxy config was not changed; installation stopped." >&2
        return 76
      fi
      ;;
    configured)
      echo "$label settings already use proxy: $proxy_url"
      return 0
      ;;
  esac

  app_proxy_backup_file_if_exists "$path" || return 1
  app_proxy_json_env_set_proxy "$path" "$proxy_url" || return 1
  echo "$label settings updated: $path"
}

app_proxy_configure_env_file_proxy() {
  local label="$1"
  local path="$2"
  local proxy_url="$3"
  local status

  status="$(app_proxy_env_file_status "$path" "$proxy_url")"
  case "$status" in
    missing)
      if ! app_proxy_prompt_yes_no_default_yes "$label env file not found. Create it at $path?"; then
        echo "$label env file was not created; installation stopped." >&2
        return 76
      fi
      ;;
    conflict)
      if ! app_proxy_prompt_yes_no_default_yes "$label env file already contains proxy variables. Overwrite them with $proxy_url?"; then
        echo "$label env file was not changed; installation stopped." >&2
        return 76
      fi
      ;;
    configured)
      echo "$label env file already uses proxy: $proxy_url"
      return 0
      ;;
  esac

  app_proxy_backup_file_if_exists "$path" || return 1
  app_proxy_env_file_set_proxy "$path" "$proxy_url" || return 1
  echo "$label env file updated: $path"
}

app_proxy_configure_toml_proxy() {
  local label="$1"
  local path="$2"
  local proxy_url="$3"
  local status

  status="$(app_proxy_toml_status "$path" "$proxy_url")"
  case "$status" in
    missing)
      if ! app_proxy_prompt_yes_no_default_yes "$label config file not found. Create it at $path?"; then
        echo "$label config file was not created; installation stopped." >&2
        return 76
      fi
      ;;
    conflict)
      if ! app_proxy_prompt_yes_no_default_yes "$label config already contains shell proxy variables. Overwrite them with $proxy_url?"; then
        echo "$label config was not changed; installation stopped." >&2
        return 76
      fi
      ;;
    configured)
      echo "$label config already uses proxy: $proxy_url"
      return 0
      ;;
  esac

  app_proxy_backup_file_if_exists "$path" || return 1
  app_proxy_toml_set_proxy "$path" "$proxy_url" || return 1
  echo "$label config updated: $path"
}

app_proxy_configure_target_official_proxy() {
  local target_app="$1"
  local display_name="$2"
  local bundle_id="$3"
  local proxy_host="$4"
  local proxy_port="$5"
  local proxy_url
  local kind
  local home_dir

  proxy_url="$(app_proxy_proxy_url "$proxy_host" "$proxy_port")"
  kind="$(app_proxy_target_kind "$target_app" "$display_name" "$bundle_id")"
  home_dir="$(app_proxy_home)"

  case "$kind" in
    claude)
      echo
      echo "Claude official config:"
      echo "  Updating Claude Code user settings env with $proxy_url."
      app_proxy_configure_json_env_proxy "Claude Code" "$home_dir/.claude/settings.json" "$proxy_url" || return $?
      ;;
    codex)
      echo
      echo "Codex official config:"
      echo "  Updating Codex env file and shell_environment_policy.set with $proxy_url."
      app_proxy_configure_env_file_proxy "Codex" "$home_dir/.codex/.env" "$proxy_url" || return $?
      app_proxy_configure_toml_proxy "Codex" "$home_dir/.codex/config.toml" "$proxy_url" || return $?
      ;;
  esac
}

app_proxy_verify_target_official_proxy_config() {
  local target_app="$1"
  local display_name="$2"
  local bundle_id="$3"
  local proxy_url="$4"
  local kind
  local home_dir
  local status
  local env_status
  local toml_status

  kind="$(app_proxy_target_kind "$target_app" "$display_name" "$bundle_id")"
  home_dir="$(app_proxy_home)"
  case "$kind" in
    claude)
      status="$(app_proxy_json_env_status "$home_dir/.claude/settings.json" "$proxy_url")"
      if [[ "$status" == "configured" ]]; then
        echo "  Official config: OK (Claude settings)"
      else
        echo "  Official config: FAILED (Claude settings status: $status)" >&2
        return 1
      fi
      ;;
    codex)
      env_status="$(app_proxy_env_file_status "$home_dir/.codex/.env" "$proxy_url")"
      toml_status="$(app_proxy_toml_status "$home_dir/.codex/config.toml" "$proxy_url")"
      if [[ "$env_status" == "configured" && "$toml_status" == "configured" ]]; then
        echo "  Official config: OK (Codex env + shell policy)"
      else
        echo "  Official config: FAILED (Codex env: $env_status, shell policy: $toml_status)" >&2
        return 1
      fi
      ;;
    *)
      echo "  Official config: skipped (generic app)"
      ;;
  esac
}

app_proxy_proxy_http_status() {
  local port="$1"
  local url="$2"
  local status

  status="$(app_proxy_cmd curl -x "http://127.0.0.1:$port" -sS -o /dev/null -w "%{http_code}" --max-time 20 "$url" 2>/dev/null || true)"
  status="${status//$'\n'/}"
  status="${status//$'\r'/}"
  if [[ "$status" =~ ^[2-5][0-9][0-9]$ ]]; then
    printf '%s\n' "$status"
    return 0
  fi
  return 1
}

app_proxy_verify_launcher_runtime_path() {
  local proxy_app="$1"
  local launcher_path="$proxy_app/Contents/MacOS/app-proxy-launcher"
  local output

  if [[ ! -x "$launcher_path" ]]; then
    echo "  Launcher runtime: FAILED ($launcher_path)" >&2
    return 1
  fi

  if output="$(APP_PROXY_HOME="$(app_proxy_home)" APP_PROXY_LAUNCHER_SMOKE_TEST=1 "$launcher_path" 2>&1)"; then
    echo "  Launcher runtime: OK ($output)"
  else
    echo "  Launcher runtime: FAILED ($output)" >&2
    return 1
  fi
}

app_proxy_verify_claude_domain_rules_if_needed() {
  local target_app="$1"
  local kind
  local config_path

  kind="$(app_proxy_detect_target_kind "$target_app")"
  if [[ "$kind" != "claude" ]]; then
    return 0
  fi

  config_path="$(app_proxy_singbox_config_path)"
  local rules_outbound
  rules_outbound="$(app_proxy_singbox_claude_rules_outbound "$config_path")"
  if [[ -n "$rules_outbound" ]]; then
    echo "  Claude domain config: OK ($rules_outbound)"
  else
    echo "  Claude domain config: FAILED (expected managed Claude domain rules)" >&2
    return 1
  fi
}

app_proxy_verify_claude_domain_runtime_if_needed() {
  local target_app="$1"
  local expected_port="$2"
  local kind
  local status

  kind="$(app_proxy_detect_target_kind "$target_app")"
  if [[ "$kind" != "claude" ]]; then
    return 0
  fi

  if status="$(app_proxy_proxy_http_status "$expected_port" "https://claude.ai/")"; then
    echo "  Claude domain runtime: OK (https://claude.ai via proxy, HTTP $status)"
  else
    echo "  Claude domain runtime: FAILED (https://claude.ai via http://127.0.0.1:$expected_port)" >&2
    return 1
  fi
}

app_proxy_cleanup_target_official_proxy() {
  local target_app="$1"
  local display_name="$2"
  local bundle_id="$3"
  local proxy_url="$4"
  local kind
  local home_dir

  kind="$(app_proxy_target_kind "$target_app" "$display_name" "$bundle_id")"
  home_dir="$(app_proxy_home)"
  case "$kind" in
    claude)
      if app_proxy_prompt_yes_no_default_yes "Remove Claude official proxy config entries matching $proxy_url?"; then
        app_proxy_backup_file_if_exists "$home_dir/.claude/settings.json" || return 1
        app_proxy_json_env_remove_proxy "$home_dir/.claude/settings.json" "$proxy_url" || return 1
        echo "Claude official proxy config cleaned: $home_dir/.claude/settings.json"
      else
        echo "Skipped Claude official proxy config cleanup."
      fi
      ;;
    codex)
      if app_proxy_prompt_yes_no_default_yes "Remove Codex official proxy config entries matching $proxy_url?"; then
        app_proxy_backup_file_if_exists "$home_dir/.codex/.env" || return 1
        app_proxy_env_file_remove_proxy "$home_dir/.codex/.env" "$proxy_url" || return 1
        echo "Codex env proxy config cleaned: $home_dir/.codex/.env"
        app_proxy_backup_file_if_exists "$home_dir/.codex/config.toml" || return 1
        app_proxy_toml_remove_proxy "$home_dir/.codex/config.toml" "$proxy_url" || return 1
        echo "Codex shell proxy config cleaned: $home_dir/.codex/config.toml"
      else
        echo "Skipped Codex official proxy config cleanup."
      fi
      ;;
  esac
}

app_proxy_cleanup_known_official_proxy_configs() {
  local home_dir

  if ! app_proxy_prompt_yes_no_default_no "No Proxy app was selected. Remove known Claude/Codex proxy config entries pointing at 127.0.0.1?"; then
    echo "Skipped Claude/Codex official proxy config cleanup."
    return 0
  fi

  home_dir="$(app_proxy_home)"
  app_proxy_backup_file_if_exists "$home_dir/.claude/settings.json" || return 1
  app_proxy_json_env_remove_local_proxy "$home_dir/.claude/settings.json" || return 1
  echo "Claude local proxy config cleaned: $home_dir/.claude/settings.json"
  app_proxy_backup_file_if_exists "$home_dir/.codex/.env" || return 1
  app_proxy_env_file_remove_local_proxy "$home_dir/.codex/.env" || return 1
  echo "Codex env local proxy config cleaned: $home_dir/.codex/.env"
  app_proxy_backup_file_if_exists "$home_dir/.codex/config.toml" || return 1
  app_proxy_toml_remove_local_proxy "$home_dir/.codex/config.toml" || return 1
  echo "Codex shell local proxy config cleaned: $home_dir/.codex/config.toml"
}

app_proxy_copy_target_icon() {
  local target_app="$1"
  local proxy_resources_dir="$2"
  local icon_file
  local icon_source
  local icon_dest_name

  icon_file="$(app_proxy_plist_value "$target_app/Contents/Info.plist" CFBundleIconFile)"
  if [[ -z "$icon_file" ]]; then return 0; fi

  icon_source="$target_app/Contents/Resources/$icon_file"
  if [[ ! -f "$icon_source" && "$icon_file" != *.icns ]]; then
    icon_source="$target_app/Contents/Resources/$icon_file.icns"
  fi
  if [[ ! -f "$icon_source" ]]; then return 0; fi

  icon_dest_name="$(/usr/bin/basename "$icon_source")"
  /bin/cp "$icon_source" "$proxy_resources_dir/$icon_dest_name"
  printf '%s\n' "${icon_dest_name%.icns}"
}

app_proxy_copy_target_icon_name() {
  local target_app="$1"
  local proxy_resources_dir="$2"
  local icon_name
  local assets_source

  icon_name="$(app_proxy_plist_value "$target_app/Contents/Info.plist" CFBundleIconName)"
  if [[ -z "$icon_name" ]]; then return 0; fi

  assets_source="$target_app/Contents/Resources/Assets.car"
  if [[ ! -f "$assets_source" ]]; then return 0; fi

  /bin/cp "$assets_source" "$proxy_resources_dir/Assets.car"
  printf '%s\n' "$icon_name"
}

app_proxy_generate_wrapper() {
  local target_app="$1"
  local proxy_host="$2"
  local proxy_port="$3"
  local target_plist="$target_app/Contents/Info.plist"
  local display_name
  local proxy_path_component
  local proxy_app_name
  local bundle_id
  local target_executable
  local hash
  local proxy_bundle_id
  local target_parent
  local proxy_app
  local proxy_contents_dir
  local proxy_macos_dir
  local proxy_resources_dir
  local config_dir
  local config_path
  local launch_agents_dir
  local guard_label
  local guard_plist
  local guard_path
  local launcher_path
  local copied_icon
  local copied_icon_name
  local normalized_proxy_port
  local existing_proxy_bundle_id

  normalized_proxy_port="$(app_proxy_normalize_port "$proxy_port")" || {
    echo "invalid proxy port: $proxy_port" >&2
    return 1
  }

  if [[ ! -d "$target_app" ]]; then
    echo "target app missing: $target_app" >&2
    return 1
  fi
  if [[ ! -f "$target_plist" ]]; then
    echo "target app Info.plist missing: $target_plist" >&2
    return 1
  fi

  display_name="$(app_proxy_display_name "$target_app")"
  proxy_path_component="$(app_proxy_safe_path_component "$display_name")" || return 1
  proxy_app_name="$display_name Proxy"
  bundle_id="$(app_proxy_plist_value "$target_plist" CFBundleIdentifier)"
  if [[ -z "$bundle_id" ]]; then
    echo "target app bundle identifier missing: $target_plist" >&2
    return 1
  fi
  if [[ "$bundle_id" == local.app-proxy.clone.* ]]; then
    # a clone already injects its own proxy; wrapping it would double-wrap and
    # leave a redundant guard/wrapper pair behind
    echo "target is an App Proxy clone; bind the clone itself (Manage apps -> bind) instead of wrapping it: $target_app" >&2
    return 1
  fi
  if [[ "$bundle_id" == local.app-proxy.* ]]; then
    echo "target is already an App Proxy wrapper; refusing to wrap it again: $target_app" >&2
    return 1
  fi
  target_executable="$(app_proxy_app_executable "$target_app")" || {
    echo "target app executable missing or not executable: $target_app" >&2
    return 1
  }

  hash="$(app_proxy_stable_hash "$target_app" "$bundle_id")"
  proxy_bundle_id="local.app-proxy.$hash"
  target_parent="$(/usr/bin/dirname "$target_app")"
  proxy_app="$target_parent/$proxy_path_component Proxy.app"
  proxy_contents_dir="$proxy_app/Contents"
  proxy_macos_dir="$proxy_contents_dir/MacOS"
  proxy_resources_dir="$proxy_contents_dir/Resources"
  config_dir="$(app_proxy_home)/Library/Application Support/App Proxy/$proxy_path_component Proxy"
  config_path="$config_dir/config.env"
  launch_agents_dir="$(app_proxy_home)/Library/LaunchAgents"
  guard_label="$proxy_bundle_id.guard"
  guard_plist="$launch_agents_dir/$guard_label.plist"
  guard_path="$proxy_resources_dir/app-proxy-guard"
  launcher_path="$proxy_macos_dir/app-proxy-launcher"

  if [[ "$(app_proxy_compare_path "$proxy_app")" == "$(app_proxy_compare_path "$target_app")" ]]; then
    echo "refusing to generate proxy app over target app: $proxy_app" >&2
    return 1
  fi
  if [[ -e "$proxy_app" && "$proxy_app" -ef "$target_app" ]]; then
    echo "refusing to generate proxy app over target app inode: $proxy_app" >&2
    return 1
  fi
  if [[ -e "$proxy_app" ]]; then
    existing_proxy_bundle_id="$(app_proxy_plist_value "$proxy_app/Contents/Info.plist" CFBundleIdentifier)"
    if [[ "$existing_proxy_bundle_id" != "$proxy_bundle_id" ]]; then
      echo "refusing to overwrite app with unexpected bundle identifier: $proxy_app" >&2
      return 1
    fi
  fi

  app_proxy_configure_target_official_proxy "$target_app" "$display_name" "$bundle_id" "$proxy_host" "$normalized_proxy_port" || return $?

  /bin/mkdir -p "$proxy_macos_dir" "$proxy_resources_dir" "$config_dir" "$launch_agents_dir"
  /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-launcher-template" "$launcher_path"
  /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-guard-template" "$guard_path"
  /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-common.sh" "$proxy_resources_dir/app-proxy-common.sh"
  /bin/chmod 0755 "$launcher_path" "$guard_path"

  copied_icon="$(app_proxy_copy_target_icon "$target_app" "$proxy_resources_dir")"
  copied_icon_name="$(app_proxy_copy_target_icon_name "$target_app" "$proxy_resources_dir")"
  app_proxy_write_proxy_info_plist "$proxy_contents_dir/Info.plist" "$proxy_app_name" "$proxy_bundle_id" "$copied_icon" "$copied_icon_name"

/bin/cat >"$config_path" <<CONFIG
PROXY_SCHEME=http
PROXY_HOST=$(app_proxy_shell_quote "$proxy_host")
PROXY_PORT=$normalized_proxy_port
TARGET_APP_PATH=$(app_proxy_shell_quote "$target_app")
TARGET_EXECUTABLE=$(app_proxy_shell_quote "$target_executable")
TARGET_BUNDLE_ID=$(app_proxy_shell_quote "$bundle_id")
PROXY_APP_PATH=$(app_proxy_shell_quote "$proxy_app")
PROXY_APP_NAME=$(app_proxy_shell_quote "$proxy_app_name")
RELAUNCH_SUPPRESSION_SECONDS=20
CONFIG

  app_proxy_write_guard_launch_agent "$guard_plist" "$guard_label" "$guard_path" "$proxy_app_name"
  app_proxy_load_guard_launch_agent "$guard_label" "$guard_plist" >/dev/null || {
    echo "Warning: guard launch agent could not be loaded; it will load at next login." >&2
  }
}

app_proxy_json_get() {
  local json="$1"
  local key="$2"

  /usr/bin/osascript -l JavaScript -e '
function run(argv) {
  try {
    var value = JSON.parse(argv[0])[argv[1]];
    if (value === undefined || value === null) return "";
    return String(value);
  } catch (error) {
    return "";
  }
}
' "$json" "$key"
}

app_proxy_trim_dragged_path() {
  local path="$1"
  local unescaped_path=""
  local index
  local path_length
  local char

  path="$(/usr/bin/sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' <<<"$path")"
  if [[ "${#path}" -ge 2 ]]; then
    if [[ "${path:0:1}" == "'" && "${path: -1}" == "'" ]]; then
      path="${path:1:${#path}-2}"
    elif [[ "${path:0:1}" == '"' && "${path: -1}" == '"' ]]; then
      path="${path:1:${#path}-2}"
    fi
  fi

  path_length="${#path}"
  for ((index = 0; index < path_length; index++)); do
    char="${path:index:1}"
    if [[ "$char" == "\\" ]] && (( index + 1 < path_length )); then
      index=$((index + 1))
      unescaped_path+="${path:index:1}"
    else
      unescaped_path+="$char"
    fi
  done

  printf '%s\n' "$unescaped_path"
}

app_proxy_prompt_for_target_app() {
  local port="$1"
  local exit_ip="${2:-}"
  local target_app
  local generate_status
  local rules_status

  echo "Drag the target .app here, then press Return:"

  if ! IFS= read -r target_app; then
    echo "no app path provided" >&2
    return 1
  fi
  target_app="$(app_proxy_trim_dragged_path "$target_app")"
  if [[ -z "$target_app" ]]; then
    echo "app path is empty" >&2
    return 1
  fi

  app_proxy_preflight_claude_singbox_rules_if_needed "$target_app" || {
    rules_status=$?
    return "$rules_status"
  }

  app_proxy_generate_wrapper "$target_app" "127.0.0.1" "$port" || {
    generate_status=$?
    return "$generate_status"
  }
  app_proxy_apply_claude_singbox_rules_if_needed "$target_app" "$port" || {
    rules_status=$?
    return "$rules_status"
  }
  echo "Proxy wrapper generated for: $target_app"
  app_proxy_finalize_install "$target_app" "$port" "$exit_ip"
}

app_proxy_report_singbox_state() {
  local port="$1"
  local exit_ip="$2"

  echo "sing-box is running"
  echo "Proxy endpoint: http://127.0.0.1:$port"
  if [[ -n "$exit_ip" ]]; then
    echo "Exit IP: $exit_ip"
  else
    echo "Exit IP: unavailable"
  fi
}

app_proxy_wait_for_singbox_http_inbound() {
  local attempts="${APP_PROXY_SINGBOX_WAIT_ATTEMPTS:-20}"
  local attempt
  local state_json=""
  local running
  local usable
  local port

  for ((attempt = 1; attempt <= attempts; attempt++)); do
    state_json="$(app_proxy_singbox_state_json)"
    running="$(app_proxy_json_get "$state_json" running)"
    usable="$(app_proxy_json_get "$state_json" usable)"
    port="$(app_proxy_json_get "$state_json" listen_port)"
    if [[ "$running" == "true" && "$usable" == "true" && -n "$port" ]]; then
      printf '%s\n' "$state_json"
      return 0
    fi
    /bin/sleep 1
  done

  printf '%s\n' "$state_json"
  return 1
}

app_proxy_report_singbox_unusable_failure() {
  local state_json="$1"
  local port
  local config_path

  port="$(app_proxy_json_get "$state_json" listen_port)"
  config_path="$(app_proxy_json_get "$state_json" config)"
  echo "sing-box local HTTP inbound is running, but proxy egress test failed." >&2
  if [[ -n "$port" ]]; then
    echo "Proxy endpoint tested: http://127.0.0.1:$port" >&2
  fi
  if [[ -n "$config_path" ]]; then
    echo "Config checked: $config_path" >&2
  fi
  echo "The installer will not continue until traffic through this proxy can reach the internet." >&2
}

app_proxy_report_singbox_start_failure() {
  local state_json="$1"
  local port
  local config_path

  port="$(app_proxy_json_get "$state_json" listen_port)"
  config_path="$(app_proxy_json_get "$state_json" config)"
  echo "sing-box did not start with a local HTTP inbound" >&2
  if [[ -n "$config_path" ]]; then
    echo "Config checked: $config_path" >&2
  fi
  if [[ -n "$port" ]]; then
    echo "Expected local HTTP inbound: http://127.0.0.1:$port" >&2
  else
    echo "No local HTTP inbound was found in the sing-box config." >&2
  fi
}

app_proxy_apply_generated_singbox_config() {
  local generated_config_file="$1"
  local state_json

  if ! app_proxy_write_singbox_config "$generated_config_file"; then
    return 1
  fi

  echo "Command: sing-box check -c $(app_proxy_singbox_config_path)"
  if ! app_proxy_check_singbox_config; then
    echo "sing-box config check failed." >&2
    return 1
  fi

  echo "Command: brew services restart sing-box"
  if ! app_proxy_restart_singbox; then
    return 1
  fi

  if state_json="$(app_proxy_wait_for_singbox_http_inbound)"; then
    APP_PROXY_LAST_SINGBOX_STATE_JSON="$state_json"
    return 0
  fi

  APP_PROXY_LAST_SINGBOX_STATE_JSON="$state_json"
  if [[ "$(app_proxy_json_get "$state_json" running)" == "true" && -n "$(app_proxy_json_get "$state_json" listen_port)" ]]; then
    return 66
  fi
  return 65
}

app_proxy_restore_singbox_config_backup() {
  local backup_path="$1"
  local config_path="$2"

  if [[ -n "$backup_path" && -f "$backup_path" ]]; then
    /bin/cp "$backup_path" "$config_path" || return 1
    echo "  Restored sing-box config backup: $backup_path"
  fi
}

app_proxy_preflight_claude_singbox_rules_if_needed() {
  local target_app="$1"
  local kind
  local config_path

  kind="$(app_proxy_detect_target_kind "$target_app")"
  if [[ "$kind" != "claude" ]]; then
    return 0
  fi

  if app_proxy_manifest_exists; then
    if [[ "$(app_proxy_manifest_profile_count)" == "0" ]]; then
      echo
      echo "Claude sing-box domain rules:"
      echo "  manifest has no profiles; cannot target Claude domain rules" >&2
      return 77
    fi
    return 0
  fi

  config_path="$(app_proxy_singbox_config_path)"
  if [[ ! -f "$config_path" ]]; then
    echo
    echo "Claude sing-box domain rules:"
    echo "  sing-box config missing; cannot add Claude domain rules: $config_path" >&2
    return 1
  fi

  if ! app_proxy_singbox_has_managed_proxy_auto "$config_path"; then
    echo
    echo "Claude sing-box domain rules:"
    app_proxy_report_no_managed_proxy_auto
    return 77
  fi
}

app_proxy_manifest_profile_count() {
  app_proxy_manifest_read | /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");
function run() {
  var data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
  var text = $.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding);
  try {
    var manifest = JSON.parse(ObjC.unwrap(text));
    return String((manifest.profiles || []).length);
  } catch (error) {
    return "0";
  }
}
'
}

app_proxy_manifest_profile_for_port() {
  local port="$1"
  app_proxy_manifest_read | /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");
function run(argv) {
  var data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
  var text = $.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding);
  try {
    var manifest = JSON.parse(ObjC.unwrap(text));
    var port = Number(argv[0]);
    var profiles = manifest.profiles || [];
    for (var i = 0; i < profiles.length; i++) {
      if (Number(profiles[i].listen_port) === port) {
        return String(profiles[i].id);
      }
    }
    return "";
  } catch (error) {
    return "";
  }
}
' "$port"
}

app_proxy_apply_claude_singbox_rules_if_needed() {
  local target_app="$1"
  local port="${2:-}"
  local kind
  local config_path
  local backup_path
  local outbound_tag
  local state_json
  local profile_id

  kind="$(app_proxy_detect_target_kind "$target_app")"
  if [[ "$kind" != "claude" ]]; then
    return 0
  fi

  if app_proxy_manifest_exists; then
    echo
    echo "Claude sing-box domain rules:"
    profile_id="$(app_proxy_manifest_profile_for_port "$port")"
    if [[ -z "$profile_id" ]]; then
      echo "  no manifest profile listens on port $port; cannot target Claude domain rules" >&2
      return 77
    fi
    app_proxy_manifest_set_setting claude_rules_profile "$profile_id" || return 1
    echo "  Domain suffix rules: anthropic.com, clau.de, claude.ai, claudeusercontent.com, claude-api.com, claudecontentmoderation.com, claudemcpclient.com"
    echo "  Domain keyword rules: anthropic, claude"
    echo "  Rule outbound: $profile_id-auto"
    app_proxy_apply_manifest_config
    return $?
  fi

  echo
  echo "Claude sing-box domain rules:"
  config_path="$(app_proxy_singbox_config_path)"
  if [[ ! -f "$config_path" ]]; then
    echo "  sing-box config missing; cannot add Claude domain rules: $config_path" >&2
    return 1
  fi

  if ! app_proxy_singbox_has_managed_proxy_auto "$config_path"; then
    app_proxy_report_no_managed_proxy_auto
    return 77
  fi

  backup_path="$(app_proxy_backup_singbox_config)" || return 1
  if [[ -n "$backup_path" ]]; then
    echo "  Backup created: $backup_path"
  fi

  if ! outbound_tag="$(app_proxy_patch_singbox_claude_rules "$config_path")"; then
    echo "  Failed to update Claude domain rules in sing-box config." >&2
    return 1
  fi
  if [[ "$outbound_tag" == "__APP_PROXY_NO_MANAGED_PROXY_AUTO__" ]]; then
    app_proxy_report_no_managed_proxy_auto
    return 77
  fi
  echo "  Domain suffix rules: anthropic.com, clau.de, claude.ai, claudeusercontent.com, claude-api.com, claudecontentmoderation.com, claudemcpclient.com"
  echo "  Domain keyword rules: anthropic, claude"
  echo "  Rule outbound: $outbound_tag"

  echo "Command: sing-box check -c $config_path"
  if ! app_proxy_check_singbox_config; then
    echo "sing-box config check failed after adding Claude domain rules." >&2
    app_proxy_restore_singbox_config_backup "$backup_path" "$config_path" || true
    return 1
  fi

  echo "Command: brew services restart sing-box"
  if ! app_proxy_restart_singbox; then
    app_proxy_restore_singbox_config_backup "$backup_path" "$config_path" || true
    return 1
  fi

  if state_json="$(app_proxy_wait_for_singbox_http_inbound)"; then
    APP_PROXY_LAST_SINGBOX_STATE_JSON="$state_json"
    return 0
  fi

  APP_PROXY_LAST_SINGBOX_STATE_JSON="$state_json"
  if [[ "$(app_proxy_json_get "$state_json" running)" == "true" && -n "$(app_proxy_json_get "$state_json" listen_port)" ]]; then
    app_proxy_report_singbox_unusable_failure "$state_json"
    if app_proxy_restore_singbox_config_backup "$backup_path" "$config_path"; then
      app_proxy_restart_singbox >/dev/null 2>&1 || true
    fi
    return 66
  fi
  app_proxy_report_singbox_start_failure "$state_json"
  if app_proxy_restore_singbox_config_backup "$backup_path" "$config_path"; then
    app_proxy_restart_singbox >/dev/null 2>&1 || true
  fi
  return 65
}

app_proxy_load_guard_launch_agent() {
  local guard_label="$1"
  local guard_plist="$2"
  local domain

  if [[ ! -f "$guard_plist" ]]; then
    echo "missing LaunchAgent plist: $guard_plist" >&2
    return 1
  fi

  if [[ "${APP_PROXY_DRY_RUN_LAUNCHCTL:-0}" == "1" ]]; then
    printf 'dry-run'
    return 0
  fi

  domain="gui/$(/usr/bin/id -u)"
  /bin/launchctl bootout "$domain" "$guard_plist" >/dev/null 2>&1 || true
  /bin/launchctl bootstrap "$domain" "$guard_plist" || return 1
  /bin/launchctl enable "$domain/$guard_label" >/dev/null 2>&1 || true
  /bin/launchctl print "$domain/$guard_label" >/dev/null 2>&1 || return 1
  printf 'loaded'
}

app_proxy_wait_for_success_exit() {
  APP_PROXY_SUCCESS_PROMPT_SHOWN=1
  echo
  printf 'All checks passed. Press any key to close this window.'
  if [[ "${APP_PROXY_NONINTERACTIVE:-0}" != "1" ]]; then
    IFS= read -r -n 1 _ || true
  fi
  printf '\n'
}

app_proxy_finalize_install() {
  local target_app="$1"
  local expected_port="$2"
  local previous_exit_ip="${3:-}"
  local target_plist="$target_app/Contents/Info.plist"
  local display_name
  local proxy_path_component
  local proxy_app_name
  local bundle_id
  local hash
  local proxy_bundle_id
  local target_parent
  local proxy_app
  local config_path
  local guard_label
  local guard_plist
  local guard_path
  local state_json
  local running
  local usable
  local state_port
  local exit_ip
  local guard_status
  local proxy_url

  display_name="$(app_proxy_display_name "$target_app")"
  proxy_path_component="$(app_proxy_safe_path_component "$display_name")" || return 1
  proxy_app_name="$display_name Proxy"
  bundle_id="$(app_proxy_plist_value "$target_plist" CFBundleIdentifier)"
  hash="$(app_proxy_stable_hash "$target_app" "$bundle_id")"
  proxy_bundle_id="local.app-proxy.$hash"
  target_parent="$(/usr/bin/dirname "$target_app")"
  proxy_app="$target_parent/$proxy_path_component Proxy.app"
  config_path="$(app_proxy_home)/Library/Application Support/App Proxy/$proxy_path_component Proxy/config.env"
  guard_label="$proxy_bundle_id.guard"
  guard_plist="$(app_proxy_home)/Library/LaunchAgents/$guard_label.plist"
  guard_path="$proxy_app/Contents/Resources/app-proxy-guard"

  state_json="$(app_proxy_singbox_state_json)"
  running="$(app_proxy_json_get "$state_json" running)"
  usable="$(app_proxy_json_get "$state_json" usable)"
  state_port="$(app_proxy_json_get "$state_json" listen_port)"
  exit_ip="$(app_proxy_json_get "$state_json" exit_ip)"
  if [[ -z "$exit_ip" ]]; then
    exit_ip="$previous_exit_ip"
  fi
  proxy_url="$(app_proxy_proxy_url "127.0.0.1" "$expected_port")"

  echo
  echo "Full-chain status:"
  if [[ "$running" == "true" ]]; then
    echo "  sing-box process: OK"
  else
    echo "  sing-box process: FAILED" >&2
    return 1
  fi

  if [[ "$usable" == "true" && "$state_port" == "$expected_port" ]]; then
    echo "  Proxy check: OK (http://127.0.0.1:$expected_port)"
  else
    echo "  Proxy check: FAILED (expected usable http://127.0.0.1:$expected_port)" >&2
    return 1
  fi

  if [[ -n "$exit_ip" ]]; then
    echo "  Exit IP: $exit_ip"
  else
    echo "  Exit IP: unavailable" >&2
    return 1
  fi

  if [[ -d "$proxy_app" ]]; then
    echo "  Proxy app: OK ($proxy_app)"
  else
    echo "  Proxy app: FAILED ($proxy_app)" >&2
    return 1
  fi

  if [[ -f "$config_path" ]]; then
    echo "  Config: OK ($config_path)"
    echo "  Edit config: open $(app_proxy_shell_quote "$config_path")"
  else
    echo "  Config: FAILED ($config_path)" >&2
    return 1
  fi

  app_proxy_verify_target_official_proxy_config "$target_app" "$display_name" "$bundle_id" "$proxy_url" || return 1
  app_proxy_verify_claude_domain_rules_if_needed "$target_app" || return 1
  app_proxy_verify_claude_domain_runtime_if_needed "$target_app" "$expected_port" || return 1
  app_proxy_verify_launcher_runtime_path "$proxy_app" || return 1

  if [[ -x "$guard_path" ]]; then
    echo "  Guard executable: OK ($guard_path)"
  else
    echo "  Guard executable: FAILED ($guard_path)" >&2
    return 1
  fi

  if [[ -f "$guard_plist" ]]; then
    echo "  Guard LaunchAgent: OK ($guard_plist)"
  else
    echo "  Guard LaunchAgent: FAILED ($guard_plist)" >&2
    return 1
  fi

  if guard_status="$(app_proxy_load_guard_launch_agent "$guard_label" "$guard_plist")"; then
    echo "  Guard launchctl: OK ($guard_status)"
  else
    echo "  Guard launchctl: FAILED ($guard_label)" >&2
    return 1
  fi

  echo
  echo "Install complete."
  echo "  Open '$proxy_app_name' to launch through the proxy."
  echo "  Runtime config: $config_path"
  echo "  Edit runtime config: open $(app_proxy_shell_quote "$config_path")"
  echo "  The guard is installed for direct starts of the original app."
  echo
  echo "Full chain is green. Proxy helmet buckled, traffic shoes tied, Claude has adult supervision. (ง •̀_•́)ง"
  echo "Open '$proxy_app_name' and let the tunnel do tunnel things. Please do not poke the original app button."
  app_proxy_wait_for_success_exit
}

app_proxy_jxa_json_file() {
  local script="$1"
  local json_file="$2"
  shift 2

  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");
function readJson(path) {
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  if (!text) throw new Error("unable to read JSON file: " + path);
  return JSON.parse(ObjC.unwrap(text));
}
' -e "$script" "$json_file" "$@"
}

app_proxy_country_list() {
  local parsed_json_file="$1"

  app_proxy_jxa_json_file '
function run(argv) {
  var payload = readJson(argv[0]);
  var nodes = Array.isArray(payload.nodes) ? payload.nodes : [];
  var countries = [];
  var counts = {};
  nodes.forEach(function(node) {
    var country = String(node.country || "Unknown");
    if (counts[country] === undefined) {
      countries.push(country);
      counts[country] = 0;
    }
    counts[country]++;
  });
  return countries.map(function(country, index) {
    return String(index + 1) + ") " + country + " (" + counts[country] + " nodes)";
  }).join("\n");
}
' "$parsed_json_file"
}

app_proxy_country_choices() {
  local parsed_json_file="$1"

  app_proxy_jxa_json_file '
function run(argv) {
  var payload = readJson(argv[0]);
  var nodes = Array.isArray(payload.nodes) ? payload.nodes : [];
  var countries = [];
  var counts = {};
  nodes.forEach(function(node) {
    var country = String(node.country || "Unknown");
    if (counts[country] === undefined) {
      countries.push(country);
      counts[country] = 0;
    }
    counts[country]++;
  });
  return countries.map(function(country) {
    return country + " (" + counts[country] + " nodes)";
  }).join("\n");
}
' "$parsed_json_file"
}

app_proxy_nodes_for_country() {
  local parsed_json_file="$1"
  local country_index="$2"

  app_proxy_jxa_json_file '
function run(argv) {
  var payload = readJson(argv[0]);
  var wantedIndex = Number(argv[1]);
  var nodes = Array.isArray(payload.nodes) ? payload.nodes : [];
  var countries = [];
  var seen = {};
  nodes.forEach(function(node) {
    var country = String(node.country || "Unknown");
    if (!seen[country]) {
      countries.push(country);
      seen[country] = true;
    }
  });
  if (!Number.isInteger(wantedIndex) || wantedIndex < 1 || wantedIndex > countries.length) {
    throw new Error("invalid country selection");
  }
  var country = countries[wantedIndex - 1];
  var selected = nodes.filter(function(node) {
    return String(node.country || "Unknown") === country;
  });
  return selected.map(function(node, index) {
    return String(index + 1) + ") " + String(node.name || node.server || "node") + " [" + String(node.protocol || "unknown") + "]";
  }).join("\n");
}
' "$parsed_json_file" "$country_index"
}

app_proxy_node_choices_for_country() {
  local parsed_json_file="$1"
  local country_index="$2"

  app_proxy_jxa_json_file '
function run(argv) {
  var payload = readJson(argv[0]);
  var wantedIndex = Number(argv[1]);
  var nodes = Array.isArray(payload.nodes) ? payload.nodes : [];
  var countries = [];
  var seen = {};
  nodes.forEach(function(node) {
    var country = String(node.country || "Unknown");
    if (!seen[country]) {
      countries.push(country);
      seen[country] = true;
    }
  });
  if (!Number.isInteger(wantedIndex) || wantedIndex < 1 || wantedIndex > countries.length) {
    throw new Error("invalid country selection");
  }
  var country = countries[wantedIndex - 1];
  var selected = nodes.filter(function(node) {
    return String(node.country || "Unknown") === country;
  });
  return selected.map(function(node) {
    return String(node.name || node.server || "node") + " [" + String(node.protocol || "unknown") + "]";
  }).join("\n");
}
' "$parsed_json_file" "$country_index"
}

app_proxy_networksetup_hardware_ports() {
  if [[ -n "${APP_PROXY_TEST_NETWORKSETUP_OUTPUT+x}" ]]; then
    printf '%s\n' "$APP_PROXY_TEST_NETWORKSETUP_OUTPUT"
    return 0
  fi
  /usr/sbin/networksetup -listallhardwareports 2>/dev/null || true
}

app_proxy_ifconfig_list() {
  if [[ -n "${APP_PROXY_TEST_IFCONFIG_LIST+x}" ]]; then
    printf '%s\n' "$APP_PROXY_TEST_IFCONFIG_LIST"
    return 0
  fi
  /sbin/ifconfig -l 2>/dev/null || true
}

app_proxy_ifconfig_info() {
  local interface_name="$1"
  local env_name

  env_name="APP_PROXY_TEST_IFCONFIG_${interface_name//[^A-Za-z0-9_]/_}"
  if [[ -n "${!env_name+x}" ]]; then
    printf '%s\n' "${!env_name}"
    return 0
  fi
  /sbin/ifconfig "$interface_name" 2>/dev/null || true
}

app_proxy_interface_names() {
  app_proxy_ifconfig_list | /usr/bin/tr ' ' '\n' | /usr/bin/awk 'NF && $0 != "lo0"'
}

app_proxy_interface_candidate() {
  local interface_name="$1"

  case "$interface_name" in
    ""|lo*|utun*|tun*|tap*|ipsec*|ppp*|gif*|stf*|bridge*|awdl*|llw*|anpi*|ap*|vmenet*|vmnet*)
      return 1
      ;;
  esac
  return 0
}

app_proxy_interface_active() {
  local interface_name="$1"
  local info

  info="$(app_proxy_ifconfig_info "$interface_name")"
  [[ -n "$info" ]] || return 1
  printf '%s\n' "$info" | /usr/bin/grep -q 'inet '
}

app_proxy_interface_ipv4() {
  local interface_name="$1"
  local info

  info="$(app_proxy_ifconfig_info "$interface_name")"
  printf '%s\n' "$info" | /usr/bin/awk '/^[[:space:]]*inet / { print $2; exit }'
}

app_proxy_interface_connected() {
  local interface_name="$1"
  local env_name

  env_name="APP_PROXY_TEST_INTERFACE_CONNECTED_${interface_name//[^A-Za-z0-9_]/_}"
  if [[ -n "${!env_name+x}" ]]; then
    [[ "${!env_name}" == "1" ]]
    return $?
  fi

  app_proxy_interface_candidate "$interface_name" || return 1
  app_proxy_interface_active "$interface_name" || return 1

  app_proxy_cmd curl --noproxy '*' --interface "$interface_name" -fsS --connect-timeout 2 --max-time 5 \
    http://captive.apple.com/hotspot-detect.html >/dev/null 2>&1
}

app_proxy_hardware_interfaces() {
  local wanted_kind="$1"

  app_proxy_networksetup_hardware_ports | /usr/bin/awk -v wanted="$wanted_kind" '
    /^Hardware Port: / {
      hardware=$0
      sub(/^Hardware Port: /, "", hardware)
      next
    }
    /^Device: / {
      device=$0
      sub(/^Device: /, "", device)
      lower=tolower(hardware)
      is_wifi=(lower == "wi-fi" || lower == "wifi" || lower == "airport")
      is_virtual=(lower ~ /(vpn|tunnel|utun|ppp|bridge|virtual|bluetooth|thunderbolt bridge)/)
      if (wanted == "wifi" && is_wifi) {
        print device
      } else if (wanted == "physical" && !is_wifi && !is_virtual && device ~ /^en[0-9]+$/) {
        print device
      }
    }
  '
}

app_proxy_first_active_interface() {
  local interface_name

  while IFS= read -r interface_name; do
    if app_proxy_interface_candidate "$interface_name" && app_proxy_interface_connected "$interface_name"; then
      printf '%s\n' "$interface_name"
      return 0
    fi
  done
  return 1
}

app_proxy_first_active_interface_from_text() {
  local interfaces="$1"

  printf '%s\n' "$interfaces" | app_proxy_first_active_interface || true
}

app_proxy_detect_upstream_interface() {
  local interface_name
  local interfaces

  if [[ -n "${APP_PROXY_DEFAULT_UPSTREAM_INTERFACE:-}" ]]; then
    printf '%s\n' "$APP_PROXY_DEFAULT_UPSTREAM_INTERFACE"
    return 0
  fi

  interfaces="$(app_proxy_hardware_interfaces wifi)"
  interface_name="$(app_proxy_first_active_interface_from_text "$interfaces")"
  if [[ -n "$interface_name" ]]; then
    printf '%s\n' "$interface_name"
    return 0
  fi

  interfaces="$(app_proxy_hardware_interfaces physical)"
  interface_name="$(app_proxy_first_active_interface_from_text "$interfaces")"
  if [[ -n "$interface_name" ]]; then
    printf '%s\n' "$interface_name"
    return 0
  fi

  printf '\n'
}

app_proxy_normalize_interface_name() {
  local interface_name="$1"

  if [[ -z "$interface_name" ]]; then
    echo "upstream interface is required" >&2
    return 1
  fi
  if [[ ! "$interface_name" =~ ^[A-Za-z0-9._-]+$ ]]; then
    echo "invalid upstream interface: $interface_name" >&2
    return 1
  fi
  printf '%s\n' "$interface_name"
}

app_proxy_choose_upstream_interface() {
  local detected_interface
  local choices
  local names
  local choice_index
  local selected_interface
  local interface_name

  APP_PROXY_UPSTREAM_INTERFACE=""
  APP_PROXY_UPSTREAM_INTERFACE_MODE="auto-binding"

  # detection order: active connected Wi-Fi first, then a connected physical NIC
  detected_interface="$(app_proxy_detect_upstream_interface)"

  if [[ -n "$detected_interface" && "${APP_PROXY_NONINTERACTIVE:-0}" == "1" ]]; then
    APP_PROXY_UPSTREAM_INTERFACE="$detected_interface"
    APP_PROXY_UPSTREAM_INTERFACE_MODE="detected"
    return 0
  fi

  names=""
  while IFS= read -r interface_name; do
    [[ -n "$interface_name" ]] || continue
    [[ "$interface_name" == "$detected_interface" ]] && continue
    names+="$interface_name"$'\n'
  done <<<"$(app_proxy_interface_names)"
  names="${names%$'\n'}"

  if [[ -n "$detected_interface" ]]; then
    choices="$detected_interface (detected, internet OK)"$'\n'"Auto binding"
  else
    echo "No active Wi-Fi or physical interface was detected." >&2
    echo "Choose Auto binding to let sing-box detect the route interface, or select an interface manually." >&2
    choices="Auto binding"
  fi
  if [[ -n "$names" ]]; then
    choices+=$'\n'"$names"
  fi

  while true; do
    if app_proxy_tui_available; then
      choice_index="$(app_proxy_select_single "Choose upstream interface (Enter = default):" "$choices")" || return $?
    else
      echo "Choose upstream interface:" >&2
      local display_index=0
      local display_line
      while IFS= read -r display_line; do
        display_index=$((display_index + 1))
        if [[ "$display_index" -eq 1 ]]; then
          echo "$display_index) $display_line [default]" >&2
        else
          echo "$display_index) $display_line" >&2
        fi
      done <<<"$choices"
      printf 'Interface [1]: ' >&2
      IFS= read -r choice_index || choice_index=""
      choice_index="${choice_index:-1}"
    fi

    if [[ ! "$choice_index" =~ ^[0-9]+$ || "$choice_index" -lt 1 ]]; then
      echo "invalid upstream interface selection: $choice_index" >&2
      return 1
    fi

    if [[ "$choice_index" -eq 1 && -n "$detected_interface" ]]; then
      APP_PROXY_UPSTREAM_INTERFACE="$detected_interface"
      APP_PROXY_UPSTREAM_INTERFACE_MODE="detected"
      return 0
    fi

    selected_interface="$(printf '%s\n' "$choices" | /usr/bin/sed -n "${choice_index}p")"
    if [[ -z "$selected_interface" ]]; then
      echo "invalid upstream interface selection: $choice_index" >&2
      return 1
    fi
    if [[ "$selected_interface" == "Auto binding" ]]; then
      APP_PROXY_UPSTREAM_INTERFACE=""
      APP_PROXY_UPSTREAM_INTERFACE_MODE="auto-binding"
      return 0
    fi

    interface_name="$(app_proxy_normalize_interface_name "$selected_interface")" || return 1
    if app_proxy_interface_connected "$interface_name"; then
      APP_PROXY_UPSTREAM_INTERFACE="$interface_name"
      APP_PROXY_UPSTREAM_INTERFACE_MODE="manual"
      return 0
    fi

    echo "Interface $interface_name has no internet connectivity right now." >&2
    if app_proxy_prompt_yes_no_default_no "Use $interface_name anyway?" >&2; then
      APP_PROXY_UPSTREAM_INTERFACE="$interface_name"
      APP_PROXY_UPSTREAM_INTERFACE_MODE="manual"
      return 0
    fi
    echo "Pick another interface or Auto binding." >&2
  done
}

app_proxy_prompt_upstream_interface() {
  app_proxy_choose_upstream_interface || return $?
  printf '%s\n' "$APP_PROXY_UPSTREAM_INTERFACE"
}

app_proxy_tui_available() {
  [[ "${APP_PROXY_NONINTERACTIVE:-0}" != "1" && -t 0 && -r /dev/tty && -w /dev/tty ]]
}

app_proxy_tui_hide_cursor() {
  printf '\033[?25l\033[?7l' >/dev/tty
}

app_proxy_tui_show_cursor() {
  printf '\033[?7h\033[?25h' >/dev/tty
}

app_proxy_tui_rows() {
  local rows
  rows="$(/bin/stty size </dev/tty 2>/dev/null | /usr/bin/awk '{print $1}')"
  if [[ "$rows" =~ ^[0-9]+$ ]] && (( rows >= 6 )); then
    printf '%s\n' "$rows"
  else
    printf '24\n'
  fi
}

app_proxy_tui_window_top() {
  # $1 cursor, $2 count, $3 visible, $4 current top -> new top keeping cursor visible
  local cursor="$1" count="$2" visible="$3" top="$4"
  (( cursor < top )) && top=$cursor
  (( cursor >= top + visible )) && top=$(( cursor - visible + 1 ))
  local max_top=$(( count - visible ))
  (( max_top < 0 )) && max_top=0
  (( top > max_top )) && top=$max_top
  (( top < 0 )) && top=0
  printf '%s\n' "$top"
}

app_proxy_select_single() {
  local title="$1"
  local choices_text="$2"
  local -a choices=()
  local choice
  local cursor=0
  local top=0
  local rendered_lines=0
  local key
  local rest
  local index
  local pointer
  local marker
  local rows
  local visible
  local window_end

  while IFS= read -r choice; do
    [[ -n "$choice" ]] && choices+=("$choice")
  done <<<"$choices_text"
  if [[ "${#choices[@]}" -eq 0 ]]; then
    echo "no choices available" >&2
    return 1
  fi

  app_proxy_tui_hide_cursor
  while true; do
    rows="$(app_proxy_tui_rows)"
    visible=$(( rows - 3 ))
    (( visible < 3 )) && visible=3
    (( visible > ${#choices[@]} )) && visible=${#choices[@]}
    top="$(app_proxy_tui_window_top "$cursor" "${#choices[@]}" "$visible" "$top")"
    window_end=$(( top + visible ))

    if (( rendered_lines > 0 )); then
      printf '\033[%dA' "$rendered_lines" >/dev/tty
    fi
    printf '\033[2K\r%s\n' "$title" >/dev/tty
    if (( ${#choices[@]} > visible )); then
      printf '\033[2K\rUse ↑/↓ to move, Return to confirm.  (%d/%d)\n' "$((cursor + 1))" "${#choices[@]}" >/dev/tty
    else
      printf '\033[2K\rUse ↑/↓ to move, Return to confirm.\n' >/dev/tty
    fi
    for ((index = top; index < window_end; index++)); do
      pointer=" "
      marker="○"
      if (( index == cursor )); then
        pointer="›"
        marker="●"
      fi
      if (( index == top && top > 0 )); then
        printf '\033[2K\r%s %s %s  ↑\n' "$pointer" "$marker" "${choices[$index]}" >/dev/tty
      elif (( index == window_end - 1 && window_end < ${#choices[@]} )); then
        printf '\033[2K\r%s %s %s  ↓\n' "$pointer" "$marker" "${choices[$index]}" >/dev/tty
      else
        printf '\033[2K\r%s %s %s\n' "$pointer" "$marker" "${choices[$index]}" >/dev/tty
      fi
    done
    rendered_lines=$(( visible + 2 ))

    if ! IFS= read -r -s -n 1 key </dev/tty; then
      app_proxy_tui_show_cursor
      return 1
    fi
    case "$key" in
      $'\033')
        IFS= read -r -s -n 2 rest </dev/tty || rest=""
        case "$rest" in
          "[A") ((cursor > 0)) && cursor=$((cursor - 1)) ;;
          "[B") ((cursor + 1 < ${#choices[@]})) && cursor=$((cursor + 1)) ;;
        esac
        ;;
      k|K)
        ((cursor > 0)) && cursor=$((cursor - 1))
        ;;
      j|J)
        ((cursor + 1 < ${#choices[@]})) && cursor=$((cursor + 1))
        ;;
      "")
        app_proxy_tui_show_cursor
        printf '%s\n' "$((cursor + 1))"
        return 0
        ;;
      $'\r'|$'\n')
        app_proxy_tui_show_cursor
        printf '%s\n' "$((cursor + 1))"
        return 0
        ;;
    esac
  done
}

app_proxy_select_multi() {
  local title="$1"
  local choices_text="$2"
  local -a choices=()
  local -a selected=()
  local choice
  local cursor=0
  local top=0
  local rendered_lines=0
  local key
  local rest
  local index
  local pointer
  local marker
  local selected_count
  local output=""
  local rows
  local visible
  local window_end

  while IFS= read -r choice; do
    if [[ -n "$choice" ]]; then
      choices+=("$choice")
      selected+=(0)
    fi
  done <<<"$choices_text"
  if [[ "${#choices[@]}" -eq 0 ]]; then
    echo "no choices available" >&2
    return 1
  fi

  app_proxy_tui_hide_cursor
  while true; do
    selected_count=0
    for ((index = 0; index < ${#selected[@]}; index++)); do
      (( selected[index] == 1 )) && selected_count=$((selected_count + 1))
    done

    rows="$(app_proxy_tui_rows)"
    visible=$(( rows - 3 ))
    (( visible < 3 )) && visible=3
    (( visible > ${#choices[@]} )) && visible=${#choices[@]}
    top="$(app_proxy_tui_window_top "$cursor" "${#choices[@]}" "$visible" "$top")"
    window_end=$(( top + visible ))

    if (( rendered_lines > 0 )); then
      printf '\033[%dA' "$rendered_lines" >/dev/tty
    fi
    printf '\033[2K\r%s\n' "$title" >/dev/tty
    if (( ${#choices[@]} > visible )); then
      printf '\033[2K\rUse ↑/↓ to move, Space to select, Return to confirm. Selected: %d  (%d/%d)\n' "$selected_count" "$((cursor + 1))" "${#choices[@]}" >/dev/tty
    else
      printf '\033[2K\rUse ↑/↓ to move, Space to select, Return to confirm. Selected: %d\n' "$selected_count" >/dev/tty
    fi
    for ((index = top; index < window_end; index++)); do
      pointer=" "
      marker="○"
      if (( selected[index] == 1 )); then
        marker="●"
      fi
      if (( index == cursor )); then
        pointer="›"
      fi
      if (( index == top && top > 0 )); then
        printf '\033[2K\r%s %s %s  ↑\n' "$pointer" "$marker" "${choices[$index]}" >/dev/tty
      elif (( index == window_end - 1 && window_end < ${#choices[@]} )); then
        printf '\033[2K\r%s %s %s  ↓\n' "$pointer" "$marker" "${choices[$index]}" >/dev/tty
      else
        printf '\033[2K\r%s %s %s\n' "$pointer" "$marker" "${choices[$index]}" >/dev/tty
      fi
    done
    rendered_lines=$(( visible + 2 ))

    if ! IFS= read -r -s -n 1 key </dev/tty; then
      app_proxy_tui_show_cursor
      return 1
    fi
    case "$key" in
      $'\033')
        IFS= read -r -s -n 2 rest </dev/tty || rest=""
        case "$rest" in
          "[A") ((cursor > 0)) && cursor=$((cursor - 1)) ;;
          "[B") ((cursor + 1 < ${#choices[@]})) && cursor=$((cursor + 1)) ;;
        esac
        ;;
      k|K)
        ((cursor > 0)) && cursor=$((cursor - 1))
        ;;
      j|J)
        ((cursor + 1 < ${#choices[@]})) && cursor=$((cursor + 1))
        ;;
      " ")
        if (( selected[cursor] == 1 )); then
          selected[cursor]=0
        else
          selected[cursor]=1
        fi
        ;;
      "")
        if (( selected_count == 0 )); then
          printf '\a' >/dev/tty
          continue
        fi
        for ((index = 0; index < ${#selected[@]}; index++)); do
          if (( selected[index] == 1 )); then
            if [[ -n "$output" ]]; then output+=","; fi
            output+="$((index + 1))"
          fi
        done
        app_proxy_tui_show_cursor
        printf '%s\n' "$output"
        return 0
        ;;
      $'\r'|$'\n')
        if (( selected_count == 0 )); then
          printf '\a' >/dev/tty
          continue
        fi
        for ((index = 0; index < ${#selected[@]}; index++)); do
          if (( selected[index] == 1 )); then
            if [[ -n "$output" ]]; then output+=","; fi
            output+="$((index + 1))"
          fi
        done
        app_proxy_tui_show_cursor
        printf '%s\n' "$output"
        return 0
        ;;
    esac
  done
}

app_proxy_ensure_homebrew() {
  local install_command
  local brew_bin

  if app_proxy_brew --version >/dev/null 2>&1; then
    return 0
  fi

  echo "Homebrew is required but not installed."
  echo "Installing it now needs your administrator password once; Homebrew itself runs as your normal user (never as root)."

  install_command='NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"'
  if [[ -t 0 ]]; then
    # terminal session: pre-warm sudo so the installer does not re-prompt
    echo "Command: sudo -v"
    if ! app_proxy_cmd sudo -v; then
      echo "Administrator authorization failed; cannot install Homebrew." >&2
      return 71
    fi
  else
    # GUI session (double-clicked .command without a usable tty): authorize
    # through the macOS credentials dialog instead of a sudo prompt
    if ! app_proxy_cmd osascript -e 'do shell script "/usr/bin/sudo -v" with administrator privileges' >/dev/null; then
      echo "Administrator authorization failed; cannot install Homebrew." >&2
      return 71
    fi
  fi

  echo "Command: $install_command"
  if ! app_proxy_cmd bash -c "$install_command"; then
    echo "Homebrew installation failed." >&2
    echo "Install Homebrew manually or rerun this installer from an administrator account." >&2
    return 71
  fi

  # make brew usable in this very process so the flow continues without a rerun
  for brew_bin in /opt/homebrew/bin/brew /usr/local/bin/brew; do
    if [[ -x "$brew_bin" ]]; then
      eval "$("$brew_bin" shellenv)"
      break
    fi
  done
  if ! app_proxy_brew --version >/dev/null 2>&1; then
    echo "Homebrew was installed but is still not usable in this session." >&2
    echo "Open a new terminal and rerun this step." >&2
    return 71
  fi
}

app_proxy_ensure_singbox_environment() {
  # prerequisite for installing nodes / creating profiles: brew + sing-box
  app_proxy_ensure_homebrew || return $?
  if app_proxy_brew list sing-box >/dev/null 2>&1; then
    return 0
  fi
  echo "Command: brew install sing-box"
  app_proxy_brew install sing-box || return 1
}

app_proxy_download_subscription() {
  local subscription_url="$1"
  local output_file="$2"
  local error_file="$output_file.err"
  local user_agent
  local last_error=""

  for user_agent in \
    "Clash.Meta" \
    "Loon/3.2.0" \
    "Quantumult X/1.5.0" \
    "Surge/5.0" \
    "Shadowrocket/2.2.0" \
    "ClashforWindows/0.20.39" \
    "ClashX Pro/1.118.1" \
    "Mozilla/5.0"; do
    if app_proxy_cmd curl -fsSL \
      -A "$user_agent" \
      -H "Accept: */*" \
      "$subscription_url" >"$output_file" 2>"$error_file"; then
      /bin/rm -f "$error_file"
      return 0
    fi
    if [[ -s "$error_file" ]]; then
      last_error="$(/usr/bin/tail -n 1 "$error_file")"
    fi
  done

  echo "subscription download failed." >&2
  if [[ -n "$last_error" ]]; then
    echo "curl: $last_error" >&2
  fi
  echo "The subscription server refused the request or did not return a downloadable subscription." >&2
  echo "If this is a provider-specific URL, try the Clash subscription endpoint or regenerate the subscription link." >&2
  /bin/rm -f "$error_file"
  return 1
}

app_proxy_apply_manifest_config() {
  local tmp_config
  local status

  tmp_config="$(/usr/bin/mktemp "${TMPDIR:-/tmp}/app-proxy-generated.XXXXXX")" || return 1
  if ! app_proxy_manifest_read | /usr/bin/osascript -l JavaScript "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-config-generator.jxa" >"$tmp_config"; then
    /bin/rm -f "$tmp_config"
    echo "config generation from manifest failed" >&2
    return 1
  fi

  if app_proxy_apply_generated_singbox_config "$tmp_config"; then
    status=0
  else
    status=$?
  fi
  /bin/rm -f "$tmp_config"
  return "$status"
}

app_proxy_cli_regenerate() {
  local status

  if app_proxy_apply_manifest_config; then
    status=0
  else
    status=$?
  fi

  if [[ "$status" -eq 65 ]]; then
    app_proxy_report_singbox_start_failure "$APP_PROXY_LAST_SINGBOX_STATE_JSON"
  elif [[ "$status" -eq 66 ]]; then
    app_proxy_report_singbox_unusable_failure "$APP_PROXY_LAST_SINGBOX_STATE_JSON"
  fi
  return "$status"
}

app_proxy_cli_nodes_json_from_parsed() {
  local parsed_file="$1"
  local country_index="$2"
  local node_indexes="$3"

  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function run(argv) {
  var text = $.NSString.stringWithContentsOfFileEncodingError(argv[0], $.NSUTF8StringEncoding, null);
  if (!text || (typeof text.isNil === "function" && text.isNil())) {
    throw new Error("parsed subscription missing: " + argv[0]);
  }
  var parsed = JSON.parse(ObjC.unwrap(text));
  var nodes = (parsed.nodes || []).filter(function (node) {
    return !!node;
  });
  var countries = [];
  var seen = {};
  nodes.forEach(function (node) {
    var country = String(node.country || "Unknown");
    if (!seen[country]) {
      seen[country] = true;
      countries.push(country);
    }
  });
  var selectedNames = {};
  if (argv[1] !== "" && argv[2] !== "") {
    var countryIndex = Number(argv[1]);
    var country = countries[countryIndex - 1];
    if (!country) {
      throw new Error("invalid country index: " + argv[1]);
    }
    var countryNodes = nodes.filter(function (node) {
      return String(node.country || "Unknown") === country;
    });
    String(argv[2]).split(",").forEach(function (raw) {
      var index = Number(raw.trim());
      var node = countryNodes[index - 1];
      if (!node) {
        throw new Error("invalid node index: " + raw);
      }
      if (node.status && node.status !== "supported") {
        throw new Error("node is not supported by the config generator: " + String(node.name || raw));
      }
      selectedNames[String(node.name)] = true;
    });
  }
  var result = nodes.filter(function (node) {
    return !node.status || node.status === "supported";
  }).map(function (node) {
    var copy = JSON.parse(JSON.stringify(node));
    delete copy.status;
    copy.selected = !!selectedNames[String(copy.name)];
    return copy;
  });
  return JSON.stringify(result);
}
' "$parsed_file" "$country_index" "$node_indexes"
}

# --- wrapper/clone config scanning ---------------------------------------------

app_proxy_cli_wrapper_config_paths() {
  local dir
  local config

  dir="$(app_proxy_home)/Library/Application Support/App Proxy"
  for config in "$dir"/*/config.env; do
    [[ -f "$config" ]] || continue
    printf '%s\n' "$config"
  done
}

app_proxy_cli_wrapper_field() {
  local config="$1"
  local field="$2"
  (
    set +u
    # shellcheck disable=SC1090
    . "$config" >/dev/null 2>&1
    eval "printf '%s' \"\${$field:-}\""
  )
}

app_proxy_cli_list_bindings() {
  local config
  local name
  local port
  local target

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    name="$(/usr/bin/basename "$(/usr/bin/dirname "$config")")"
    port="$(app_proxy_cli_wrapper_field "$config" PROXY_PORT)"
    target="$(app_proxy_cli_wrapper_field "$config" TARGET_APP_PATH)"
    printf '%s\t%s\t%s\n' "$name" "$port" "$target"
  done < <(app_proxy_cli_wrapper_config_paths)
}

app_proxy_cli_wrapper_dependents() {
  local port="$1"
  local config
  local wrapper_port

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    wrapper_port="$(app_proxy_cli_wrapper_field "$config" PROXY_PORT)"
    if [[ "$wrapper_port" == "$port" ]]; then
      printf '%s\n' "$(/usr/bin/basename "$(/usr/bin/dirname "$config")")"
    fi
  done < <(app_proxy_cli_wrapper_config_paths)
}

app_proxy_cli_wrapper_dependent_configs() {
  local port="$1"
  local config
  local wrapper_port

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    wrapper_port="$(app_proxy_cli_wrapper_field "$config" PROXY_PORT)"
    if [[ "$wrapper_port" == "$port" ]]; then
      printf '%s\n' "$config"
    fi
  done < <(app_proxy_cli_wrapper_config_paths)
}

# --- app clones (isolated instances of Chromium/Electron apps) ------------------

app_proxy_clone_validate_name() {
  local name="$1"

  if [[ ! "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
    echo "clone name must start with a letter/digit and contain only letters, digits, . _ -: $name" >&2
    return 1
  fi
}

app_proxy_clone_home_dir() {
  # deliberately short: unix socket paths are capped at 103 bytes on macOS and
  # Electron/Chromium apps bind sockets under $HOME (e.g. VS Code's
  # .../Code/1.12-main.sock); a home under ~/Library/Application Support/...
  # pushes them over the limit and the apps exit on launch
  printf '%s/.app-proxy/%s\n' "$(app_proxy_home)" "$1"
}

app_proxy_clone_config_for_name() {
  local name="$1"
  local config
  local clone_name

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    clone_name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
    if [[ "$clone_name" == "$name" ]]; then
      printf '%s\n' "$config"
      return 0
    fi
  done < <(app_proxy_cli_wrapper_config_paths)
  return 1
}

app_proxy_clone_list() {
  # tab columns: name, display, mode, port, target, clone_home
  local config
  local clone_name
  local mode
  local port
  local target
  local clone_home

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    clone_name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
    [[ -n "$clone_name" ]] || continue
    mode="$(app_proxy_cli_wrapper_field "$config" CLONE_MODE)"
    [[ -n "$mode" ]] || mode="light"
    port="$(app_proxy_cli_wrapper_field "$config" PROXY_PORT)"
    target="$(app_proxy_cli_wrapper_field "$config" TARGET_APP_PATH)"
    clone_home="$(app_proxy_cli_wrapper_field "$config" CLONE_HOME)"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$clone_name" "$(/usr/bin/basename "$(/usr/bin/dirname "$config")")" "$mode" "${port:-direct}" "$target" "$clone_home"
  done < <(app_proxy_cli_wrapper_config_paths)
}

app_proxy_clone_copy_initial_data() {
  local target_app="$1"
  local clone_name="$2"
  local source_home
  local clone_home
  local bundle_name
  local kind
  local data_dir

  source_home="$(app_proxy_home)"
  clone_home="$(app_proxy_clone_home_dir "$clone_name")"
  bundle_name="$(app_proxy_plist_value "$target_app/Contents/Info.plist" CFBundleName)"
  kind="$(app_proxy_detect_target_kind "$target_app")"

  /bin/mkdir -p "$clone_home/Library/Application Support" || return 1
  data_dir="$source_home/Library/Application Support/$bundle_name"
  if [[ -n "$bundle_name" && -d "$data_dir" ]]; then
    /bin/cp -R "$data_dir" "$clone_home/Library/Application Support/" || return 1
    echo "Copied app data: $data_dir"
  fi
  if [[ "$kind" == "claude" && -d "$source_home/.claude" ]]; then
    /bin/cp -R "$source_home/.claude" "$clone_home/.claude" || return 1
    echo "Copied Claude CLI data: $source_home/.claude"
  fi
  if [[ "$kind" == "codex" && -d "$source_home/.codex" ]]; then
    /bin/cp -R "$source_home/.codex" "$clone_home/.codex" || return 1
    echo "Copied Codex CLI data: $source_home/.codex"
  fi
}

app_proxy_clone_plist_identity() {
  # rewrite identity keys on a copied original Info.plist (other keys retained,
  # e.g. ElectronAsarIntegrity, so runtime integrity checks keep passing)
  local plist="$1"
  local bundle_id="$2"
  local display="$3"
  local key

  /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $bundle_id" "$plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Add :CFBundleIdentifier string $bundle_id" "$plist" || return 1
  for key in CFBundleName CFBundleDisplayName; do
    /usr/libexec/PlistBuddy -c "Set :$key $display" "$plist" 2>/dev/null \
      || /usr/libexec/PlistBuddy -c "Add :$key string $display" "$plist" || return 1
  done
  /usr/libexec/PlistBuddy -c "Set :CFBundleExecutable app-proxy-launcher" "$plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Add :CFBundleExecutable string app-proxy-launcher" "$plist" || return 1
}

app_proxy_clone_resign() {
  # ad-hoc, non-hardened re-sign: the identity rewrite breaks the original
  # signature, and natively signed apps (e.g. Codex) carry provisioning-backed
  # entitlements that make AMFI kill any binary whose seal no longer matches.
  # Dropping the hardened runtime also keeps library validation off, so the
  # ad-hoc main executable can still load the vendor-signed frameworks.
  local clone_app="$1"
  local macos_dir="$clone_app/Contents/MacOS"
  local entry
  local entry_name

  # every file in Contents/MacOS gets a fresh ad-hoc signature: the copied
  # target binary's original one binds the (now rewritten) Info.plist and
  # restricted entitlements, and codesign refuses to seal a bundle whose
  # MacOS dir contains unsigned files (our helper scripts).
  # app-proxy-launcher is the bundle main executable: the bundle sign covers it.
  for entry in "$macos_dir"/*; do
    [[ -f "$entry" && ! -L "$entry" ]] || continue
    entry_name="$(/usr/bin/basename "$entry")"
    [[ "$entry_name" == "app-proxy-launcher" ]] && continue
    /usr/bin/codesign --force --sign - "$entry" 2>/dev/null || return 1
  done
  /usr/bin/codesign --force --sign - "$clone_app" 2>/dev/null
}

app_proxy_clone_runtime_manifest() {
  # $1=clone app path $2=mode -> tab rows: embedded-file<TAB>template-name.
  # Clones embed copies of these scripts at creation time; later template
  # fixes (e.g. the "Keychain Not Found" bridge) never reach existing clones
  # on their own — the shadow self-heal deliberately preserves them.
  local app="$1"
  local mode="$2"

  printf '%s\t%s\n' "$app/Contents/MacOS/app-proxy-launcher" "app-proxy-launcher-template"
  if [[ "$mode" == "shadow" ]]; then
    printf '%s\t%s\n' "$app/Contents/MacOS/app-proxy-guard" "app-proxy-guard-template"
    printf '%s\t%s\n' "$app/Contents/MacOS/app-proxy-common.sh" "app-proxy-common.sh"
  else
    printf '%s\t%s\n' "$app/Contents/Resources/app-proxy-guard" "app-proxy-guard-template"
    printf '%s\t%s\n' "$app/Contents/Resources/app-proxy-common.sh" "app-proxy-common.sh"
  fi
}

app_proxy_repair_stale_clone_runtimes() {
  # doctor check: list clones whose embedded runtime scripts differ from the
  # installed templates and repair them (default: yes). Returns 0 when nothing
  # is stale or everything was repaired; 1 when stale clones remain.
  local config
  local name
  local app
  local mode
  local dest
  local tmpl
  local stale
  local stale_rows=""
  local count=0
  local failed=0

  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
    [[ -n "$name" ]] || continue
    app="$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)"
    mode="$(app_proxy_cli_wrapper_field "$config" CLONE_MODE)"
    [[ -n "$mode" ]] || mode="light"
    [[ -n "$app" && -d "$app" ]] || continue
    stale=""
    while IFS=$'\t' read -r dest tmpl; do
      [[ -n "$dest" ]] || continue
      if ! /usr/bin/cmp -s "$APP_PROXY_INSTALLER_CORE_DIR/$tmpl" "$dest"; then
        stale="$stale${stale:+, }$(/usr/bin/basename "$dest")"
      fi
    done < <(app_proxy_clone_runtime_manifest "$app" "$mode")
    if [[ -n "$stale" ]]; then
      stale_rows="$stale_rows$name"$'\t'"$app"$'\t'"$mode"$'\t'"$stale"$'\n'
      count=$((count + 1))
    fi
  done < <(app_proxy_cli_wrapper_config_paths)

  [[ "$count" -gt 0 ]] || return 0

  echo
  echo "Clones with outdated embedded runtime scripts (missing later fixes,"
  echo "e.g. the 'Keychain Not Found' keychain bridge):"
  while IFS=$'\t' read -r name app mode stale; do
    [[ -n "$name" ]] || continue
    echo "  $name  [$stale]"
  done <<<"$stale_rows"

  if ! app_proxy_prompt_yes_no_default_yes "Repair these $count clone(s) with the current scripts?"; then
    echo "Left $count stale clone(s) in place."
    return 1
  fi

  while IFS=$'\t' read -r name app mode stale; do
    [[ -n "$name" ]] || continue
    while IFS=$'\t' read -r dest tmpl; do
      [[ -n "$dest" ]] || continue
      if ! /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/$tmpl" "$dest" \
         || ! /bin/chmod 0755 "$dest"; then
        echo "  failed to repair $name: cannot write $dest" >&2
        failed=1
        continue 2
      fi
    done < <(app_proxy_clone_runtime_manifest "$app" "$mode")
    # replacing sealed bundle contents invalidates the ad-hoc signature of
    # shadow clones; re-sign or AMFI kills the app on next launch
    if [[ "$mode" == "shadow" ]] && ! app_proxy_clone_resign "$app"; then
      echo "  failed to re-sign repaired clone: $app" >&2
      failed=1
      continue
    fi
    echo "Repaired clone runtime: $name (quit and relaunch the app to pick it up)"
  done <<<"$stale_rows"
  return "$failed"
}

app_proxy_generate_clone() {
  local target_app="$1"
  local clone_name="$2"
  local proxy_port="${3:-}"
  local copy_data="${4:-}"
  local clone_mode="${5:-light}"
  local target_plist="$target_app/Contents/Info.plist"
  local display_name
  local target_bundle_name
  local clone_display
  local path_component
  local clone_bundle_id
  local target_bundle_id
  local target_executable
  local clone_app
  local contents_dir
  local macos_dir
  local resources_dir
  local config_dir
  local config_path
  local clone_home
  local normalized_port=""
  local proxy_host="127.0.0.1"
  local copied_icon
  local copied_icon_name

  app_proxy_clone_validate_name "$clone_name" || return 1
  if [[ ! -d "$target_app" || ! -f "$target_plist" ]]; then
    echo "target app missing or invalid: $target_app" >&2
    return 1
  fi
  if app_proxy_clone_config_for_name "$clone_name" >/dev/null 2>&1; then
    echo "a clone named '$clone_name' already exists" >&2
    return 1
  fi
  if [[ -n "$proxy_port" ]]; then
    normalized_port="$(app_proxy_normalize_port "$proxy_port")" || {
      echo "invalid clone proxy port: $proxy_port" >&2
      return 1
    }
  fi

  display_name="$(app_proxy_display_name "$target_app")"
  target_bundle_name="$(app_proxy_plist_value "$target_plist" CFBundleName)"
  clone_display="$display_name · $clone_name"
  path_component="$(app_proxy_safe_path_component "$clone_display")" || return 1
  target_bundle_id="$(app_proxy_plist_value "$target_plist" CFBundleIdentifier)"
  if [[ -z "$target_bundle_id" ]]; then
    echo "target app bundle identifier missing: $target_plist" >&2
    return 1
  fi
  target_executable="$(app_proxy_app_executable "$target_app")" || {
    echo "target app executable missing or not executable: $target_app" >&2
    return 1
  }
  clone_bundle_id="local.app-proxy.clone.$(app_proxy_stable_hash "$target_app" "$clone_name")"

  clone_app="$(/usr/bin/dirname "$target_app")/$path_component.app"
  if [[ -e "$clone_app" ]]; then
    echo "clone app already exists: $clone_app" >&2
    return 1
  fi
  contents_dir="$clone_app/Contents"
  macos_dir="$contents_dir/MacOS"
  resources_dir="$contents_dir/Resources"
  config_dir="$(app_proxy_cli_support_dir)/$path_component"
  config_path="$config_dir/config.env"
  clone_home="$(app_proxy_clone_home_dir "$clone_name")"

  local runtime_dir
  local exec_name
  local exec_copy=""

  if [[ "$clone_mode" == "shadow" ]]; then
    # full clone: own bundle identity so it can run ALONGSIDE the original.
    # The whole app is copied (APFS clonefile: instant, copy-on-write) because
    # Chromium/Electron helper-process sandboxes resolve symlinks back to the
    # original bundle path and refuse to load frameworks through them.
    runtime_dir="$macos_dir"
    if ! /bin/cp -Rc "$target_app" "$clone_app" 2>/dev/null; then
      /bin/rm -rf "$clone_app"
      /bin/cp -R "$target_app" "$clone_app" || return 1
    fi
    /bin/mkdir -p "$config_dir" "$clone_home" || return 1
    /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-launcher-template" "$macos_dir/app-proxy-launcher" || return 1
    /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-guard-template" "$macos_dir/app-proxy-guard" || return 1
    /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-common.sh" "$macos_dir/app-proxy-common.sh" || return 1
    /bin/chmod 0755 "$macos_dir/app-proxy-launcher" "$macos_dir/app-proxy-guard"
    # per-app-named guard symlink must exist BEFORE the ad-hoc re-sign so it is
    # part of the sealed bundle (write_guard later recreates it identically)
    /bin/ln -sfh "app-proxy-guard" "$macos_dir/$clone_display Guard" || return 1

    exec_name="$(/usr/bin/basename "$target_executable")"
    exec_copy="$macos_dir/$exec_name"
    if [[ ! -x "$exec_copy" ]]; then
      echo "failed to copy the target app main executable: $exec_copy" >&2
      return 1
    fi

    # provisioning profile only matches the original identity
    /bin/rm -f "$contents_dir/embedded.provisionprofile"
    app_proxy_clone_plist_identity "$contents_dir/Info.plist" "$clone_bundle_id" "$clone_display" || return 1
    if ! app_proxy_clone_resign "$clone_app"; then
      echo "failed to ad-hoc re-sign the clone app: $clone_app" >&2
      return 1
    fi
  else
    runtime_dir="$resources_dir"
    /bin/mkdir -p "$macos_dir" "$resources_dir" "$config_dir" "$clone_home" || return 1
    /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-launcher-template" "$macos_dir/app-proxy-launcher" || return 1
    /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-guard-template" "$resources_dir/app-proxy-guard" || return 1
    /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-common.sh" "$resources_dir/app-proxy-common.sh" || return 1
    /bin/chmod 0755 "$macos_dir/app-proxy-launcher" "$resources_dir/app-proxy-guard"

    copied_icon="$(app_proxy_copy_target_icon "$target_app" "$resources_dir")"
    copied_icon_name="$(app_proxy_copy_target_icon_name "$target_app" "$resources_dir")"
    app_proxy_write_proxy_info_plist "$contents_dir/Info.plist" "$clone_display" "$clone_bundle_id" "$copied_icon" "$copied_icon_name"
  fi

  if [[ "$copy_data" == "copy" ]]; then
    app_proxy_clone_copy_initial_data "$target_app" "$clone_name" || return 1
  fi

  if [[ -n "$normalized_port" ]]; then
    ( APP_PROXY_HOME="$clone_home" app_proxy_configure_target_official_proxy "$target_app" "$display_name" "$target_bundle_id" "$proxy_host" "$normalized_port" ) || return $?
  fi

  {
    if [[ -n "$normalized_port" ]]; then
      printf 'PROXY_SCHEME=http\n'
      printf 'PROXY_HOST=%s\n' "$(app_proxy_shell_quote "$proxy_host")"
      printf 'PROXY_PORT=%s\n' "$normalized_port"
    fi
    printf 'CLONE_NAME=%s\n' "$(app_proxy_shell_quote "$clone_name")"
    printf 'CLONE_MODE=%s\n' "$(app_proxy_shell_quote "$clone_mode")"
    printf 'CLONE_HOME=%s\n' "$(app_proxy_shell_quote "$clone_home")"
    # Chromium/Electron resolve their profile dir (and the single-instance
    # lock inside it) from the real user home, ignoring $HOME — pass it in
    if [[ -n "$target_bundle_name" ]]; then
      printf 'CLONE_USER_DATA_DIR=%s\n' "$(app_proxy_shell_quote "$clone_home/Library/Application Support/$target_bundle_name")"
    fi
    printf 'CLONE_GUARD_LABEL=%s\n' "$(app_proxy_shell_quote "$clone_bundle_id.guard")"
    printf 'TARGET_APP_PATH=%s\n' "$(app_proxy_shell_quote "$target_app")"
    if [[ "$clone_mode" == "shadow" ]]; then
      printf 'TARGET_EXECUTABLE=%s\n' "$(app_proxy_shell_quote "$exec_copy")"
      printf 'TARGET_SOURCE_EXECUTABLE=%s\n' "$(app_proxy_shell_quote "$target_executable")"
    else
      printf 'TARGET_EXECUTABLE=%s\n' "$(app_proxy_shell_quote "$target_executable")"
    fi
    printf 'TARGET_BUNDLE_ID=%s\n' "$(app_proxy_shell_quote "$target_bundle_id")"
    printf 'PROXY_APP_PATH=%s\n' "$(app_proxy_shell_quote "$clone_app")"
    printf 'PROXY_APP_NAME=%s\n' "$(app_proxy_shell_quote "$clone_display")"
    printf 'RELAUNCH_SUPPRESSION_SECONDS=20\n'
  } >"$config_path"

  local guard_plist
  guard_plist="$(app_proxy_home)/Library/LaunchAgents/$clone_bundle_id.guard.plist"
  /bin/mkdir -p "$(app_proxy_home)/Library/LaunchAgents" || return 1
  app_proxy_write_guard_launch_agent "$guard_plist" "$clone_bundle_id.guard" "$runtime_dir/app-proxy-guard" "$clone_display"
  app_proxy_load_guard_launch_agent "$clone_bundle_id.guard" "$guard_plist" >/dev/null || {
    echo "Warning: clone guard launch agent could not be loaded; it will load at next login." >&2
  }

  echo "Clone created: $clone_app"
  echo "  data home: $clone_home"
  if [[ -n "$normalized_port" ]]; then
    echo "  proxy: http://$proxy_host:$normalized_port"
  else
    echo "  proxy: none (direct connection)"
  fi
}

app_proxy_clone_rename() {
  local old_name="$1"
  local new_name="$2"
  local config
  local config_dir
  local target_app
  local display_name
  local new_display
  local new_component
  local old_app
  local new_app
  local new_config_dir
  local new_config

  app_proxy_clone_validate_name "$new_name" || return 1
  config="$(app_proxy_clone_config_for_name "$old_name")" || {
    echo "no clone named '$old_name'" >&2
    return 1
  }
  if app_proxy_clone_config_for_name "$new_name" >/dev/null 2>&1; then
    echo "a clone named '$new_name' already exists" >&2
    return 1
  fi

  target_app="$(app_proxy_cli_wrapper_field "$config" TARGET_APP_PATH)"
  if [[ -d "$target_app" ]]; then
    display_name="$(app_proxy_display_name "$target_app")"
  else
    display_name="$(app_proxy_cli_wrapper_field "$config" PROXY_APP_NAME)"
    display_name="${display_name% · $old_name}"
  fi
  new_display="$display_name · $new_name"
  new_component="$(app_proxy_safe_path_component "$new_display")" || return 1

  old_app="$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)"
  new_app="$(/usr/bin/dirname "$old_app")/$new_component.app"
  if [[ -e "$new_app" ]]; then
    echo "rename target app path already exists: $new_app" >&2
    return 1
  fi
  /bin/mv "$old_app" "$new_app" || return 1
  /usr/libexec/PlistBuddy -c "Set :CFBundleName $new_display" "$new_app/Contents/Info.plist" || return 1
  /usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName $new_display" "$new_app/Contents/Info.plist" || return 1
  if [[ "$(app_proxy_cli_wrapper_field "$config" CLONE_MODE)" == "shadow" ]]; then
    app_proxy_clone_resign "$new_app" || echo "Warning: could not re-sign renamed clone: $new_app" >&2
  fi

  config_dir="$(/usr/bin/dirname "$config")"
  new_config_dir="$(app_proxy_cli_support_dir)/$new_component"
  /bin/mv "$config_dir" "$new_config_dir" || return 1
  new_config="$new_config_dir/config.env"

  {
    /usr/bin/grep -v -e '^CLONE_NAME=' -e '^PROXY_APP_PATH=' -e '^PROXY_APP_NAME=' "$new_config"
    printf 'CLONE_NAME=%s\n' "$(app_proxy_shell_quote "$new_name")"
    printf 'PROXY_APP_PATH=%s\n' "$(app_proxy_shell_quote "$new_app")"
    printf 'PROXY_APP_NAME=%s\n' "$(app_proxy_shell_quote "$new_display")"
  } >"$new_config.tmp" || return 1
  /bin/mv "$new_config.tmp" "$new_config" || return 1

  echo "Clone renamed: $old_name -> $new_name ($new_app)"
  echo "  data home unchanged: $(app_proxy_cli_wrapper_field "$new_config" CLONE_HOME)"
}

app_proxy_clone_delete() {
  local name="$1"
  shift
  local purge=0
  local arg
  local config
  local clone_home
  local clone_app

  for arg in "$@"; do
    if [[ "$arg" == "--purge" ]]; then
      purge=1
    fi
  done

  config="$(app_proxy_clone_config_for_name "$name")" || {
    echo "no clone named '$name'" >&2
    return 1
  }
  clone_home="$(app_proxy_cli_wrapper_field "$config" CLONE_HOME)"
  clone_app="$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)"

  local guard_label
  local guard_plist
  guard_label="$(app_proxy_cli_wrapper_field "$config" CLONE_GUARD_LABEL)"
  if [[ -n "$guard_label" ]]; then
    guard_plist="$(app_proxy_home)/Library/LaunchAgents/$guard_label.plist"
    if [[ "${APP_PROXY_DRY_RUN_LAUNCHCTL:-0}" != "1" && -f "$guard_plist" ]]; then
      /bin/launchctl bootout "gui/$(/usr/bin/id -u)" "$guard_plist" >/dev/null 2>&1 || true
    fi
    /bin/rm -f "$guard_plist"
  fi

  if [[ -n "$clone_app" && "$clone_app" == *.app && -e "$clone_app" ]]; then
    /bin/rm -rf "$clone_app"
    echo "Removed clone app: $clone_app"
  fi
  /bin/rm -rf "$(/usr/bin/dirname "$config")"

  if [[ "$purge" -eq 1 ]]; then
    if [[ -n "$clone_home" && "$clone_home" == */.app-proxy/* ]]; then
      /bin/rm -rf "$clone_home"
      echo "Removed clone data: $clone_home"
    elif [[ -n "$clone_home" && "$clone_home" == */Clones/* ]]; then
      # legacy layout: ~/Library/Application Support/App Proxy/Clones/<name>/home
      /bin/rm -rf "$(/usr/bin/dirname "$clone_home")"
      echo "Removed clone data: $(/usr/bin/dirname "$clone_home")"
    fi
  else
    echo "Clone data kept: $clone_home"
    echo "  (delete later with: app-proxy clone delete $name --purge, or remove the directory manually)"
  fi
}

APP_PROXY_CLI_RESOURCE_FILES="app-proxy-common.sh app-proxy-singbox.sh app-proxy-installer-core.sh app-proxy-manifest.sh app-proxy-manifest.jxa app-proxy-cli.sh app-proxy-config-generator.jxa app-proxy-subscription-parser.jxa app-proxy-launcher-template app-proxy-guard-template app-proxy-refresh-agent-template app-proxy-bin-template"

APP_PROXY_REFRESH_AGENT_LABEL="com.app-proxy.refresh"

app_proxy_cli_support_dir() {
  printf '%s/Library/Application Support/App Proxy\n' "$(app_proxy_home)"
}

app_proxy_cli_bin_path() {
  printf '%s/bin/app-proxy\n' "$(app_proxy_cli_support_dir)"
}

app_proxy_cli_link_dir() {
  local brew_prefix

  if [[ -n "${APP_PROXY_CLI_LINK_DIR:-}" ]]; then
    printf '%s\n' "$APP_PROXY_CLI_LINK_DIR"
    return 0
  fi
  brew_prefix="$(app_proxy_brew --prefix 2>/dev/null || true)"
  if [[ -n "$brew_prefix" && -d "$brew_prefix/bin" ]]; then
    printf '%s/bin\n' "$brew_prefix"
    return 0
  fi
  printf '/usr/local/bin\n'
}

app_proxy_install_cli() {
  local support_dir
  local resources_dir
  local bin_path
  local link_dir
  local resource

  support_dir="$(app_proxy_cli_support_dir)"
  resources_dir="$support_dir/cli/Resources"
  bin_path="$(app_proxy_cli_bin_path)"

  /bin/mkdir -p "$resources_dir" "$support_dir/bin" "$support_dir/logs" || return 1
  for resource in $APP_PROXY_CLI_RESOURCE_FILES; do
    if [[ ! -f "$APP_PROXY_INSTALLER_CORE_DIR/$resource" ]]; then
      echo "missing CLI resource: $APP_PROXY_INSTALLER_CORE_DIR/$resource" >&2
      return 1
    fi
    /bin/cp "$APP_PROXY_INSTALLER_CORE_DIR/$resource" "$resources_dir/$resource" || return 1
  done

  /usr/bin/sed "s|__APP_PROXY_CLI_RESOURCES__|$resources_dir|" \
    "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-bin-template" >"$bin_path" || return 1
  /bin/chmod 0755 "$bin_path"

  link_dir="$(app_proxy_cli_link_dir)"
  if [[ -d "$link_dir" && -w "$link_dir" ]]; then
    /bin/ln -sf "$bin_path" "$link_dir/app-proxy" || return 1
    echo "app-proxy CLI installed: $link_dir/app-proxy"
  else
    echo "Warning: cannot write $link_dir; link the CLI manually:" >&2
    echo "  ln -s \"$bin_path\" <a directory on your PATH>/app-proxy" >&2
  fi
}

app_proxy_refresh_agent_plist() {
  printf '%s/Library/LaunchAgents/%s.plist\n' "$(app_proxy_home)" "$APP_PROXY_REFRESH_AGENT_LABEL"
}

app_proxy_install_refresh_agent() {
  local agents_dir
  local plist
  local bin_path
  local log_path
  local interval
  local domain

  agents_dir="$(app_proxy_home)/Library/LaunchAgents"
  plist="$(app_proxy_refresh_agent_plist)"
  bin_path="$(app_proxy_cli_bin_path)"
  log_path="$(app_proxy_cli_support_dir)/logs/refresh.log"
  interval="${APP_PROXY_REFRESH_INTERVAL:-86400}"

  /bin/mkdir -p "$agents_dir" "$(app_proxy_cli_support_dir)/logs" || return 1
  /usr/bin/sed \
    -e "s|__LABEL__|$APP_PROXY_REFRESH_AGENT_LABEL|" \
    -e "s|__BIN__|$bin_path|" \
    -e "s|__INTERVAL__|$interval|" \
    -e "s|__LOG__|$log_path|" \
    "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-refresh-agent-template" >"$plist" || return 1

  if [[ "${APP_PROXY_DRY_RUN_LAUNCHCTL:-0}" == "1" ]]; then
    echo "Refresh agent installed (dry-run): $plist"
    return 0
  fi

  domain="gui/$(/usr/bin/id -u)"
  /bin/launchctl bootout "$domain" "$plist" >/dev/null 2>&1 || true
  /bin/launchctl bootstrap "$domain" "$plist" || {
    echo "Warning: could not load refresh agent; it will load at next login." >&2
    return 0
  }
  /bin/launchctl enable "$domain/$APP_PROXY_REFRESH_AGENT_LABEL" >/dev/null 2>&1 || true
  echo "Refresh agent installed: $plist"
}

app_proxy_uninstall_cli() {
  local support_dir
  local plist
  local link_dir
  local domain
  local manifest_path

  support_dir="$(app_proxy_cli_support_dir)"
  plist="$(app_proxy_refresh_agent_plist)"
  link_dir="$(app_proxy_cli_link_dir)"
  manifest_path="$(app_proxy_manifest_path)"

  if [[ -f "$plist" ]]; then
    if [[ "${APP_PROXY_DRY_RUN_LAUNCHCTL:-0}" != "1" ]]; then
      domain="gui/$(/usr/bin/id -u)"
      /bin/launchctl bootout "$domain" "$plist" >/dev/null 2>&1 || true
    fi
    /bin/rm -f "$plist"
  fi

  if [[ -L "$link_dir/app-proxy" ]]; then
    /bin/rm -f "$link_dir/app-proxy"
  fi
  /bin/rm -rf "$support_dir/cli" "$support_dir/logs"
  /bin/rm -f "$(app_proxy_cli_bin_path)"
  /bin/rmdir "$support_dir/bin" 2>/dev/null || true
  /bin/rm -f "$manifest_path"
  echo "app-proxy CLI, refresh agent, and manifest removed."
}

app_proxy_setup_singbox_config() {
  local default_port
  local listen_port_input
  local listen_port
  local doh_server_input
  local doh_server
  local upstream_interface
  local upstream_interface_mode
  local apply_status
  local subscription_url
  local tmp_dir
  local subscription_file
  local parsed_json_file
  local nodes_json
  local country_index
  local node_indexes
  local country_list
  local node_list
  local country_choices
  local node_choices

  app_proxy_ensure_singbox_environment || return $?

  default_port="$(app_proxy_find_available_port 18099)" || default_port="18099"
  printf 'Listen port [%s]: ' "$default_port"
  IFS= read -r listen_port_input || listen_port_input=""
  listen_port="${listen_port_input:-$default_port}"
  listen_port="$(app_proxy_normalize_port "$listen_port")" || {
    echo "invalid listen port: $listen_port_input" >&2
    return 1
  }

  printf 'Custom DoH server [optional]: '
  IFS= read -r doh_server_input || doh_server_input=""
  doh_server="$doh_server_input"
  if [[ -n "$doh_server" ]]; then
    echo "DoH servers: $doh_server, https://dns.google/dns-query, https://cloudflare-dns.com/dns-query, https://dns.alidns.com/dns-query"
    echo "  Primary DoH server: $doh_server"
  else
    echo "DoH servers: https://dns.google/dns-query, https://cloudflare-dns.com/dns-query, https://dns.alidns.com/dns-query"
    echo "  Primary DoH server: https://dns.google/dns-query"
  fi
  echo "  DoH hostnames are bootstrapped through local DNS with IPv4-only resolution."

  app_proxy_choose_upstream_interface || return $?
  upstream_interface="$APP_PROXY_UPSTREAM_INTERFACE"
  upstream_interface_mode="$APP_PROXY_UPSTREAM_INTERFACE_MODE"
  APP_PROXY_LAST_UPSTREAM_INTERFACE_MODE="$upstream_interface_mode"
  APP_PROXY_LAST_UPSTREAM_INTERFACE="$upstream_interface"
  if [[ -n "$upstream_interface" ]]; then
    echo "Upstream interface: $upstream_interface"
    echo "  sing-box node connections will bind to this interface to avoid external VPN default routes."
    if ! app_proxy_interface_active "$upstream_interface"; then
      echo "  Warning: interface $upstream_interface is not currently active; sing-box may fail to connect until it is available." >&2
    fi
  else
    echo "Upstream interface: Auto binding"
    echo "  sing-box route.auto_detect_interface will choose the route interface."
  fi

  printf 'Subscription URL: '
  if ! IFS= read -r subscription_url || [[ -z "$subscription_url" ]]; then
    echo "subscription URL is required" >&2
    return 1
  fi

  tmp_dir="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/app-proxy-singbox-setup.XXXXXX")" || return 1
  subscription_file="$tmp_dir/subscription.txt"
  parsed_json_file="$tmp_dir/parsed.json"

  if ! app_proxy_download_subscription "$subscription_url" "$subscription_file"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi
  if ! /usr/bin/osascript -l JavaScript "$APP_PROXY_INSTALLER_CORE_DIR/app-proxy-subscription-parser.jxa" <"$subscription_file" >"$parsed_json_file"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi

  if ! country_list="$(app_proxy_country_list "$parsed_json_file")"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi
  if [[ -z "$country_list" ]]; then
    echo "subscription did not contain supported nodes" >&2
    /bin/rm -rf "$tmp_dir"
    return 1
  fi
  if app_proxy_tui_available; then
    if ! country_choices="$(app_proxy_country_choices "$parsed_json_file")"; then
      /bin/rm -rf "$tmp_dir"
      return 1
    fi
    if ! country_index="$(app_proxy_select_single "Available countries:" "$country_choices")"; then
      /bin/rm -rf "$tmp_dir"
      return 1
    fi
  else
    echo "Available countries:"
    printf '%s\n' "$country_list"
    printf 'Choose country number: '
    IFS= read -r country_index || country_index=""
  fi

  if ! node_list="$(app_proxy_nodes_for_country "$parsed_json_file" "$country_index")"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi
  if app_proxy_tui_available; then
    if ! node_choices="$(app_proxy_node_choices_for_country "$parsed_json_file" "$country_index")"; then
      /bin/rm -rf "$tmp_dir"
      return 1
    fi
    if ! node_indexes="$(app_proxy_select_multi "Available nodes:" "$node_choices")"; then
      /bin/rm -rf "$tmp_dir"
      return 1
    fi
  else
    echo "Available nodes:"
    printf '%s\n' "$node_list"
    printf 'Choose node numbers, comma-separated: '
    IFS= read -r node_indexes || node_indexes=""
  fi

  if ! nodes_json="$(app_proxy_cli_nodes_json_from_parsed "$parsed_json_file" "$country_index" "$node_indexes")"; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi

  /bin/rm -f "$(app_proxy_manifest_path)"
  if ! app_proxy_manifest_init; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi
  app_proxy_manifest_set_setting doh_server "$doh_server" || { /bin/rm -rf "$tmp_dir"; return 1; }
  app_proxy_manifest_set_setting upstream_interface "$upstream_interface" || { /bin/rm -rf "$tmp_dir"; return 1; }
  app_proxy_manifest_set_setting upstream_interface_mode "$upstream_interface_mode" || { /bin/rm -rf "$tmp_dir"; return 1; }
  if ! app_proxy_manifest_add_profile "{\"listen_port\": $listen_port, \"source\": {\"kind\": \"subscription\", \"url\": $(app_proxy_json_string "$subscription_url")}, \"nodes\": $nodes_json}" >/dev/null; then
    /bin/rm -rf "$tmp_dir"
    return 1
  fi

  if app_proxy_apply_manifest_config; then
    apply_status=0
  else
    apply_status=$?
  fi
  if [[ "$apply_status" -ne 0 ]]; then
    if [[ ( "$apply_status" -eq 65 || "$apply_status" -eq 66 ) && "$upstream_interface_mode" == "detected" && -n "$upstream_interface" ]]; then
      echo "Auto-selected interface $upstream_interface did not start sing-box successfully."
      echo "Retrying with sing-box Auto binding."
      upstream_interface=""
      upstream_interface_mode="auto-binding-fallback"
      APP_PROXY_LAST_UPSTREAM_INTERFACE=""
      APP_PROXY_LAST_UPSTREAM_INTERFACE_MODE="$upstream_interface_mode"
      app_proxy_manifest_set_setting upstream_interface "" || { /bin/rm -rf "$tmp_dir"; return 1; }
      app_proxy_manifest_set_setting upstream_interface_mode "$upstream_interface_mode" || { /bin/rm -rf "$tmp_dir"; return 1; }
      if app_proxy_apply_manifest_config; then
        apply_status=0
      else
        apply_status=$?
      fi
      if [[ "$apply_status" -ne 0 ]]; then
        if [[ "$apply_status" -eq 65 ]]; then
          app_proxy_report_singbox_start_failure "$APP_PROXY_LAST_SINGBOX_STATE_JSON"
        elif [[ "$apply_status" -eq 66 ]]; then
          app_proxy_report_singbox_unusable_failure "$APP_PROXY_LAST_SINGBOX_STATE_JSON"
        fi
        /bin/rm -rf "$tmp_dir"
        return "$apply_status"
      fi
    else
      if [[ "$apply_status" -eq 65 ]]; then
        app_proxy_report_singbox_start_failure "$APP_PROXY_LAST_SINGBOX_STATE_JSON"
      elif [[ "$apply_status" -eq 66 ]]; then
        app_proxy_report_singbox_unusable_failure "$APP_PROXY_LAST_SINGBOX_STATE_JSON"
      fi
      /bin/rm -rf "$tmp_dir"
      return "$apply_status"
    fi
  fi
  /bin/rm -rf "$tmp_dir"
}

app_proxy_install_main() {
  local state_json
  local running
  local usable
  local port
  local exit_ip
  local answer
  local config_path

  state_json="$(app_proxy_singbox_state_json)"
  running="$(app_proxy_json_get "$state_json" running)"
  usable="$(app_proxy_json_get "$state_json" usable)"
  port="$(app_proxy_json_get "$state_json" listen_port)"
  exit_ip="$(app_proxy_json_get "$state_json" exit_ip)"

  if [[ "$running" == "true" && "$usable" == "true" && -n "$port" ]]; then
    app_proxy_report_singbox_state "$port" "$exit_ip"
    app_proxy_install_cli_and_refresh_agent
    app_proxy_prompt_for_target_app "$port" "$exit_ip"
    return $?
  fi
  if [[ "$running" == "true" && -n "$port" && "$usable" != "true" ]]; then
    app_proxy_report_singbox_unusable_failure "$state_json"
    return 66
  fi

  echo "No usable sing-box proxy was detected."
  printf 'Install Homebrew and sing-box now? [Y/n] '
  IFS= read -r answer || answer=""
  case "$answer" in
    n|N)
      echo "No proxy is available; proxy flow cannot continue." >&2
      return 70
      ;;
  esac

  app_proxy_setup_singbox_config || return $?

  state_json="${APP_PROXY_LAST_SINGBOX_STATE_JSON:-}"
  if [[ -z "$state_json" ]]; then
    if ! state_json="$(app_proxy_wait_for_singbox_http_inbound)"; then
      app_proxy_report_singbox_start_failure "$state_json"
      return 65
    fi
  fi
  running="$(app_proxy_json_get "$state_json" running)"
  usable="$(app_proxy_json_get "$state_json" usable)"
  port="$(app_proxy_json_get "$state_json" listen_port)"
  exit_ip="$(app_proxy_json_get "$state_json" exit_ip)"
  if [[ "$running" != "true" || -z "$port" ]]; then
    echo "sing-box did not start with a local HTTP inbound" >&2
    return 65
  fi
  if [[ "$usable" != "true" ]]; then
    app_proxy_report_singbox_unusable_failure "$state_json"
    return 66
  fi

  app_proxy_report_singbox_state "$port" "$exit_ip"
  app_proxy_install_cli_and_refresh_agent
  app_proxy_prompt_for_target_app "$port" "$exit_ip"
}

app_proxy_install_cli_and_refresh_agent() {
  echo
  echo "app-proxy CLI:"
  if ! app_proxy_install_cli; then
    echo "  Warning: app-proxy CLI install failed; port/node management commands will be unavailable." >&2
  fi
  if ! app_proxy_install_refresh_agent; then
    echo "  Warning: subscription refresh agent install failed." >&2
  fi
}

app_proxy_cleanup_singbox_environment() {
  local config
  local backup
  local oldest_backup=""

  echo
  echo "Environment cleanup:"
  echo "  This stops the sing-box service and removes the generated config."
  echo "  Homebrew itself is not removed."
  echo "  Existing Proxy apps left in place will still launch through their configured proxy port; networking may fail until sing-box is configured again."

  config="$(app_proxy_singbox_config_path)"

  if app_proxy_brew list sing-box >/dev/null 2>&1; then
    echo "Command: brew services stop sing-box"
    if ! app_proxy_brew services stop sing-box >/dev/null 2>&1; then
      echo "  Warning: brew services stop sing-box failed or service was not running." >&2
    fi
    # the sing-box package itself is kept by default
    if app_proxy_prompt_yes_no_default_no "Also uninstall the Homebrew sing-box package (brew uninstall sing-box)?"; then
      echo "Command: brew uninstall sing-box"
      if ! app_proxy_brew uninstall sing-box >/dev/null 2>&1; then
        echo "  Warning: brew uninstall sing-box failed. You may need to remove it manually." >&2
      fi
    else
      echo "  Kept the Homebrew sing-box package (service stopped only)."
    fi
  else
    echo "  Homebrew sing-box package not found; skipping package uninstall."
  fi

  # restore the pre-install config when we have a backup of it, else delete
  for backup in "$config".bak-app-proxy-*; do
    if [[ -e "$backup" ]]; then
      oldest_backup="$backup"
      break
    fi
  done
  if [[ -f "$config" ]]; then
    if [[ -n "$oldest_backup" ]]; then
      /bin/cp "$oldest_backup" "$config"
      echo "Restored pre-install sing-box config from backup: $oldest_backup"
    else
      /bin/rm -f "$config"
      echo "Removed sing-box config: $config"
    fi
  else
    echo "sing-box config not found: $config"
  fi

  for backup in "$config".bak-app-proxy-*; do
    if [[ -e "$backup" ]]; then
      /bin/rm -f "$backup"
      echo "Removed sing-box config backup: $backup"
    fi
  done

  app_proxy_uninstall_cli
}

app_proxy_uninstall_data_dir() {
  # removes the whole App Proxy data directory (manifest, wrapper configs) and
  # the clone data homes (~/.app-proxy, plus legacy Clones/ inside the data dir)
  local data_dir
  local clone_homes_dir

  data_dir="$(app_proxy_home)/Library/Application Support/App Proxy"
  clone_homes_dir="$(app_proxy_home)/.app-proxy"
  if [[ ! -d "$data_dir" && ! -d "$clone_homes_dir" ]]; then
    return 0
  fi

  echo
  echo "App Proxy data directory: $data_dir"
  if [[ -d "$clone_homes_dir" || -d "$data_dir/Clones" ]]; then
    echo "  Clone data homes (logins, chat history) will also be deleted:"
    [[ -d "$clone_homes_dir" ]] && echo "    $clone_homes_dir"
    [[ -d "$data_dir/Clones" ]] && echo "    $data_dir/Clones"
    echo "  Deleting them is NOT recoverable."
  fi
  if app_proxy_prompt_yes_no_default_yes "Delete the App Proxy data directory (manifest, wrapper configs, clone data)?"; then
    /bin/rm -rf "$data_dir" "$clone_homes_dir"
    echo "Removed App Proxy data directory: $data_dir"
    [[ -e "$clone_homes_dir" ]] || echo "Removed clone data homes: $clone_homes_dir"
  else
    echo "Kept App Proxy data directory: $data_dir"
  fi
}

app_proxy_launchctl_guard_labels() {
  # loaded launchd jobs whose label is an app-proxy guard. These are what
  # surface in System Settings > Login Items > App Background Activity, even
  # after the backing plist/app is gone. Test seam: APP_PROXY_TEST_LAUNCHCTL_LIST
  # (set, possibly empty) stubs the loaded-label list.
  if [[ -n "${APP_PROXY_TEST_LAUNCHCTL_LIST+x}" ]]; then
    [[ -n "$APP_PROXY_TEST_LAUNCHCTL_LIST" ]] && printf '%s\n' "$APP_PROXY_TEST_LAUNCHCTL_LIST"
    return 0
  fi
  /bin/launchctl list 2>/dev/null \
    | /usr/bin/awk '$3 ~ /^local\.app-proxy\..*\.guard$/ { print $3 }'
}

app_proxy_orphan_guards() {
  # prints "<kind>\t<label>\t<plist>" for every orphan guard:
  #   file    = plist on disk whose guard executable is gone (app removed)
  #   session = launchd job still registered with no backing plist file
  local agents_dir
  local plist
  local label
  local guard_path

  agents_dir="$(app_proxy_home)/Library/LaunchAgents"

  for plist in "$agents_dir"/local.app-proxy.*.guard.plist; do
    [[ -f "$plist" ]] || continue
    label="$(/usr/bin/basename "$plist" .plist)"
    guard_path="$(/usr/libexec/PlistBuddy -c 'Print :ProgramArguments:0' "$plist" 2>/dev/null || true)"
    if [[ -z "$guard_path" || ! -e "$guard_path" ]]; then
      printf 'file\t%s\t%s\n' "$label" "$plist"
    fi
  done

  while IFS= read -r label; do
    [[ -n "$label" ]] || continue
    if [[ ! -f "$agents_dir/$label.plist" ]]; then
      printf 'session\t%s\t\n' "$label"
    fi
  done < <(app_proxy_launchctl_guard_labels)
}

app_proxy_remove_orphan_guard() {
  # $1: guard label, $2: plist path (empty for session-only orphans)
  local label="$1"
  local plist="$2"
  local domain

  domain="gui/$(/usr/bin/id -u)"
  if [[ "${APP_PROXY_DRY_RUN_LAUNCHCTL:-0}" != "1" ]]; then
    if [[ -n "$plist" ]]; then
      /bin/launchctl bootout "$domain" "$plist" >/dev/null 2>&1 || true
    fi
    /bin/launchctl bootout "$domain/$label" >/dev/null 2>&1 || true
  fi
  [[ -n "$plist" ]] && /bin/rm -f "$plist"
  return 0
}

app_proxy_sweep_orphan_guards() {
  # find orphan guards (dead plists + loaded-but-fileless login items) and,
  # after listing them, remove them (default: yes). Shared by doctor and the
  # uninstaller. Returns 0 when nothing orphaned or all cleaned; 1 when orphans
  # were found and left in place.
  local rows
  local kind
  local label
  local plist
  local count=0

  rows="$(app_proxy_orphan_guards)"
  if [[ -z "$rows" ]]; then
    return 0
  fi

  echo
  echo "Orphan guard background items (dead LaunchAgents / leftover login items):"
  while IFS=$'\t' read -r kind label plist; do
    [[ -n "$label" ]] || continue
    count=$((count + 1))
    if [[ "$kind" == "session" ]]; then
      echo "  $label  [still registered with launchd, no config file]"
    else
      echo "  $label  [dead plist: $plist]"
    fi
  done <<<"$rows"

  if ! app_proxy_prompt_yes_no_default_yes "Remove these $count orphan guard item(s)?"; then
    echo "Left $count orphan guard item(s) in place."
    return 1
  fi

  while IFS=$'\t' read -r kind label plist; do
    [[ -n "$label" ]] || continue
    app_proxy_remove_orphan_guard "$label" "$plist"
    echo "Removed orphan guard: $label"
  done <<<"$rows"
  return 0
}

app_proxy_uninstall_sweep_machine_guards() {
  # optional deep clean: every app-proxy guard LaunchAgent in this user's
  # LaunchAgents dir, including orphans from older installs. Default: skip.
  local agents_dir
  local plist
  local plists=""
  local labels=""
  local guard_path
  local state
  local indexes
  local index
  local chosen

  agents_dir="$(app_proxy_home)/Library/LaunchAgents"
  for plist in "$agents_dir"/local.app-proxy.*.guard.plist; do
    [[ -f "$plist" ]] || continue
    plists="$plists$plist"$'\n'
    guard_path="$(/usr/libexec/PlistBuddy -c 'Print :ProgramArguments:0' "$plist" 2>/dev/null || true)"
    if [[ -n "$guard_path" && -e "$guard_path" ]]; then
      state="active"
    else
      state="ORPHAN (target app missing)"
    fi
    labels="$labels$(/usr/bin/basename "$plist")  [$state]"$'\n'
  done
  plists="${plists%$'\n'}"
  labels="${labels%$'\n'}"

  if [[ -z "$plists" ]]; then
    return 0
  fi

  echo
  echo "Guard LaunchAgents found in $agents_dir:"
  printf '%s\n' "$labels" | /usr/bin/sed 's/^/  /'
  if ! app_proxy_prompt_yes_no_default_no "Clean up guard LaunchAgents machine-wide (pick which ones next)?"; then
    echo "Left guard LaunchAgents untouched."
    return 0
  fi

  if app_proxy_tui_available; then
    indexes="$(app_proxy_select_multi "Select guard LaunchAgents to remove:" "$labels")" || return 0
  else
    echo "Enter numbers to remove, comma-separated (empty = none):"
    printf '%s\n' "$labels" | /usr/bin/awk '{print "  " NR ") " $0}'
    printf 'Numbers: '
    IFS= read -r indexes || indexes=""
    indexes="${indexes//,/ }"
  fi

  for index in $indexes; do
    [[ "$index" =~ ^[0-9]+$ ]] || continue
    chosen="$(printf '%s\n' "$plists" | /usr/bin/sed -n "${index}p")"
    [[ -n "$chosen" ]] || continue
    if [[ "${APP_PROXY_DRY_RUN_LAUNCHCTL:-0}" != "1" ]]; then
      /bin/launchctl bootout "gui/$(/usr/bin/id -u)" "$chosen" >/dev/null 2>&1 || true
    fi
    /bin/rm -f "$chosen"
    echo "Removed guard LaunchAgent: $chosen"
  done
}

app_proxy_official_config_still_needed() {
  # The official proxy config (~/.codex, ~/.claude) is shared by every plain
  # wrapper of the same kind. $1: kind, $2: the config.env being removed (to
  # exclude). Returns 0 when another managed plain wrapper of the same kind
  # still relies on that official config, 1 otherwise. Clones keep their config
  # in their own CLONE_HOME, so they never hold a reference to the real one.
  local kind="$1"
  local self_config="$2"
  local config
  local ctarget
  local cdisplay
  local cbundle
  local ckind

  [[ -n "$kind" ]] || return 1
  while IFS= read -r config; do
    [[ -n "$config" ]] || continue
    [[ "$config" == "$self_config" ]] && continue
    [[ -n "$(app_proxy_cli_wrapper_field "$config" CLONE_HOME)" ]] && continue
    ctarget="$(app_proxy_cli_wrapper_field "$config" TARGET_APP_PATH)"
    [[ -n "$ctarget" ]] || continue
    cdisplay=""
    cbundle=""
    if [[ -f "$ctarget/Contents/Info.plist" ]]; then
      cdisplay="$(app_proxy_display_name "$ctarget")"
      cbundle="$(app_proxy_plist_value "$ctarget/Contents/Info.plist" CFBundleIdentifier)"
    fi
    ckind="$(app_proxy_target_kind "$ctarget" "$cdisplay" "$cbundle")"
    if [[ "$ckind" == "$kind" ]]; then
      return 0
    fi
  done < <(app_proxy_cli_wrapper_config_paths)
  return 1
}

app_proxy_uninstall_proxy_app() {
  local proxy_app="$1"
  local info_plist
  local proxy_app_name
  local proxy_path_component
  local proxy_bundle_id
  local proxy_executable
  local guard_label
  local home_dir
  local guard_plist
  local config_dir
  local config_file
  local log_dir
  local target_app_path=""
  local target_display_name=""
  local target_bundle_id=""
  local proxy_url=""

  proxy_app="$(app_proxy_trim_dragged_path "$proxy_app")"
  if [[ -z "$proxy_app" ]]; then
    echo "proxy app path is empty" >&2
    return 1
  fi
  if [[ "$proxy_app" != *.app ]]; then
    echo "proxy app path must end with .app: $proxy_app" >&2
    return 1
  fi
  if [[ ! -d "$proxy_app" ]]; then
    echo "proxy app missing: $proxy_app" >&2
    return 1
  fi

  info_plist="$proxy_app/Contents/Info.plist"
  if [[ ! -f "$info_plist" ]]; then
    echo "proxy app Info.plist missing: $info_plist" >&2
    return 1
  fi

  proxy_app_name="$(app_proxy_plist_value "$info_plist" CFBundleName)"
  if [[ -z "$proxy_app_name" ]]; then
    echo "proxy app bundle name missing: $info_plist" >&2
    return 1
  fi
  proxy_path_component="$(app_proxy_safe_path_component "$proxy_app_name")" || return 1

  proxy_bundle_id="$(app_proxy_plist_value "$info_plist" CFBundleIdentifier)"
  if [[ -z "$proxy_bundle_id" ]]; then
    echo "proxy app bundle identifier missing: $info_plist" >&2
    return 1
  fi
  if [[ ! "$proxy_bundle_id" =~ ^local\.app-proxy\.(clone\.)?[0-9a-f]{12}$ ]]; then
    echo "refusing to uninstall non-App Proxy wrapper: unexpected bundle identifier: $proxy_bundle_id" >&2
    return 1
  fi
  if [[ "$proxy_bundle_id" == local.app-proxy.clone.* ]]; then
    local clone_config
    clone_config="$(app_proxy_home)/Library/Application Support/App Proxy/$proxy_path_component/config.env"
    if [[ ! -f "$clone_config" ]]; then
      echo "clone wrapper config missing: $clone_config" >&2
      return 1
    fi
    app_proxy_uninstall_clone_by_config "$clone_config"
    return $?
  fi
  if [[ "$proxy_app_name" != *" Proxy" ]]; then
    echo "refusing to uninstall non-App Proxy wrapper: bundle name must end with ' Proxy': $proxy_app_name" >&2
    return 1
  fi
  proxy_executable="$(app_proxy_plist_value "$info_plist" CFBundleExecutable)"
  if [[ "$proxy_executable" != "app-proxy-launcher" ]]; then
    echo "refusing to uninstall non-App Proxy wrapper: unexpected executable: $proxy_executable" >&2
    return 1
  fi

  guard_label="$proxy_bundle_id.guard"
  home_dir="$(app_proxy_home)"
  guard_plist="$home_dir/Library/LaunchAgents/$guard_label.plist"
  config_dir="$home_dir/Library/Application Support/App Proxy/$proxy_path_component"
  config_file="$config_dir/config.env"
  log_dir="$home_dir/Library/Logs/App Proxy/$proxy_path_component"

  if [[ -f "$config_file" ]]; then
    # shellcheck disable=SC1090
    . "$config_file"
    target_app_path="${TARGET_APP_PATH:-}"
    if [[ -n "${PROXY_HOST:-}" && -n "${PROXY_PORT:-}" ]]; then
      proxy_url="$(app_proxy_proxy_url "$PROXY_HOST" "$PROXY_PORT")"
    fi
    if [[ -n "$target_app_path" && -f "$target_app_path/Contents/Info.plist" ]]; then
      target_display_name="$(app_proxy_display_name "$target_app_path")"
      target_bundle_id="$(app_proxy_plist_value "$target_app_path/Contents/Info.plist" CFBundleIdentifier)"
    else
      target_display_name="${proxy_app_name% Proxy}"
    fi
  fi

  if [[ "${APP_PROXY_DRY_RUN_LAUNCHCTL:-0}" != "1" && -f "$guard_plist" ]]; then
    /bin/launchctl bootout "gui/$(/usr/bin/id -u)" "$guard_plist" >/dev/null 2>&1 || true
  fi

  if [[ -n "$proxy_url" ]]; then
    local target_kind
    target_kind="$(app_proxy_target_kind "$target_app_path" "$target_display_name" "$target_bundle_id")"
    if [[ -n "$target_kind" ]] && app_proxy_official_config_still_needed "$target_kind" "$config_file"; then
      echo "Kept $target_kind official proxy config: another proxy app for it remains."
    else
      app_proxy_cleanup_target_official_proxy "$target_app_path" "$target_display_name" "$target_bundle_id" "$proxy_url" || return $?
    fi
  else
    echo "Official app proxy config cleanup skipped because wrapper config was not readable."
  fi

  if [[ -e "$guard_plist" ]]; then
    /bin/rm -f "$guard_plist"
  fi
  if [[ -e "$config_dir" ]]; then
    /bin/rm -rf "$config_dir"
  fi
  if [[ -e "$proxy_app" ]]; then
    /bin/rm -rf "$proxy_app"
  fi
  /bin/mkdir -p "$log_dir"

  echo "Removed proxy app: $proxy_app"
  echo "Removed LaunchAgent: $guard_plist"
  echo "Removed config: $config_dir"
  echo "Logs preserved: $log_dir"
}

app_proxy_uninstall_clone_by_config() {
  local config="$1"
  local clone_name
  local clone_home
  local purge_args=()

  clone_name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
  if [[ -z "$clone_name" ]]; then
    echo "not a clone config: $config" >&2
    return 1
  fi
  if [[ "${APP_PROXY_UNINSTALL_PURGE_CLONES:-ask}" == "1" ]]; then
    purge_args=(--purge)
  elif [[ "${APP_PROXY_UNINSTALL_PURGE_CLONES:-ask}" == "ask" ]]; then
    clone_home="$(app_proxy_cli_wrapper_field "$config" CLONE_HOME)"
    echo "Clone '$clone_name' data directory: ${clone_home:-unknown}"
    echo "  It contains logins/chat history; deletion is NOT recoverable."
    if app_proxy_prompt_yes_no_default_yes "Delete the clone's data directory?"; then
      purge_args=(--purge)
    fi
  fi
  app_proxy_clone_delete "$clone_name" ${purge_args[@]+"${purge_args[@]}"}
}

app_proxy_uninstall_known_targets() {
  # prints "<config path>" lines for every managed wrapper/clone
  app_proxy_cli_wrapper_config_paths
}

app_proxy_uninstall_config_entry() {
  # remove one managed instance by its config path (clone or plain wrapper)
  local config="$1"
  local clone_name

  clone_name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
  if [[ -n "$clone_name" ]]; then
    app_proxy_uninstall_clone_by_config "$config"
  else
    app_proxy_uninstall_proxy_app "$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)"
  fi
}

app_proxy_prompt_for_proxy_cleanup() {
  local required="$1"
  local proxy_app
  local configs
  local config
  local labels=""
  local clone_name
  local app_path
  local kind_label
  local indexes
  local index
  local chosen
  local status=0
  local -a config_list=()

  configs="$(app_proxy_uninstall_known_targets)"
  if [[ -n "$configs" ]]; then
    while IFS= read -r config; do
      [[ -n "$config" ]] || continue
      config_list+=("$config")
      clone_name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
      app_path="$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)"
      if [[ -n "$clone_name" ]]; then
        kind_label="clone '$clone_name'"
      else
        kind_label="proxy wrapper"
      fi
      labels="$labels$(/usr/bin/basename "$(/usr/bin/dirname "$config")") [$kind_label] ($app_path)"$'\n'
    done <<<"$configs"
    labels="${labels%$'\n'}"

    echo "Managed proxy apps and clones (you can remove several at once):"
    if app_proxy_tui_available; then
      indexes="$(app_proxy_select_multi "Select proxy apps / clones to remove:" "$labels")" || indexes=""
    else
      printf '%s\n' "$labels" | /usr/bin/awk '{print "  " NR ") " $0}'
      if [[ "$required" == "1" ]]; then
        echo "Enter numbers to remove (comma-separated), or drag a Proxy .app here, then press Return:"
      else
        echo "Enter numbers to remove (comma-separated), or drag a Proxy .app here. Press Return without input to skip:"
      fi
      IFS= read -r indexes || indexes=""
    fi

    # a dragged path (non-numeric) still works as a single-target fallback
    if [[ -n "$indexes" && ! "$indexes" =~ ^[0-9,\ ]+$ ]]; then
      proxy_app="$(app_proxy_trim_dragged_path "$indexes")"
      if [[ -n "$proxy_app" ]]; then
        app_proxy_uninstall_proxy_app "$proxy_app"
        return $?
      fi
    fi

    indexes="${indexes//,/ }"
    if [[ -z "${indexes// /}" ]]; then
      echo "No proxy app selected; skipped proxy app cleanup."
      return 0
    fi
    for index in $indexes; do
      [[ "$index" =~ ^[0-9]+$ ]] || continue
      chosen="${config_list[$((index - 1))]:-}"
      if [[ -z "$chosen" ]]; then
        echo "invalid selection: $index" >&2
        continue
      fi
      app_proxy_uninstall_config_entry "$chosen" || status=1
    done
    return "$status"
  fi

  # no managed configs recorded: fall back to a single drag-drop target
  if [[ "$required" == "1" ]]; then
    echo "Drag the Proxy .app here, then press Return:"
  else
    echo "Drag the Proxy .app here. Press Return without input to skip proxy app cleanup:"
  fi
  if ! IFS= read -r proxy_app; then
    echo "no proxy app path provided" >&2
    return 1
  fi
  proxy_app="$(app_proxy_trim_dragged_path "$proxy_app")"
  if [[ -z "$proxy_app" && "$required" != "1" ]]; then
    echo "No proxy app provided; skipped proxy app, guard, and wrapper config cleanup."
    return 0
  fi
  if [[ -z "$proxy_app" ]]; then
    echo "proxy app path is empty" >&2
    return 1
  fi

  app_proxy_uninstall_proxy_app "$proxy_app"
}

app_proxy_uninstall_everything() {
  local configs
  local config
  local clone_name
  local app_path

  configs="$(app_proxy_uninstall_known_targets)"
  if [[ -n "$configs" ]]; then
    while IFS= read -r config; do
      [[ -n "$config" ]] || continue
      clone_name="$(app_proxy_cli_wrapper_field "$config" CLONE_NAME)"
      if [[ -n "$clone_name" ]]; then
        # each clone asks individually (default: delete), printing its data path
        app_proxy_uninstall_clone_by_config "$config" || true
        continue
      fi
      app_path="$(app_proxy_cli_wrapper_field "$config" PROXY_APP_PATH)"
      if [[ -n "$app_path" && -d "$app_path" ]]; then
        app_proxy_uninstall_proxy_app "$app_path" || true
      else
        echo "Wrapper app missing on disk; removing leftover config: $(/usr/bin/dirname "$config")" >&2
        /bin/rm -rf "$(/usr/bin/dirname "$config")"
      fi
    done <<<"$configs"
  else
    echo "No managed proxy apps or clones found."
  fi
}

app_proxy_prompt_uninstall_mode() {
  local choices
  local mode

  choices=$'Clean all\nClean environment only\nClean Proxy only'

  if app_proxy_tui_available; then
    app_proxy_select_single "Choose uninstall cleanup mode:" "$choices"
    return $?
  fi

  echo "Choose uninstall cleanup mode:" >&2
  echo "1) Clean all: sing-box environment + selected Proxy app [default]" >&2
  echo "2) Clean environment only: sing-box/Homebrew package and config; leave Proxy apps and guards in place" >&2
  echo "3) Clean Proxy only: selected Proxy app, guard, and wrapper config" >&2
  printf 'Mode [1]: ' >&2
  IFS= read -r mode || mode=""
  mode="${mode:-1}"
  case "$mode" in
    1|2|3)
      printf '%s\n' "$mode"
      ;;
    *)
      echo "invalid cleanup mode: $mode" >&2
      return 1
      ;;
  esac

}

app_proxy_uninstall_main() {
  local mode

  APP_PROXY_SUCCESS_EXIT_MESSAGE="Uninstall complete."
  mode="$(app_proxy_prompt_uninstall_mode)" || return $?
  case "$mode" in
    1)
      app_proxy_uninstall_everything
      app_proxy_sweep_orphan_guards || true
      app_proxy_uninstall_sweep_machine_guards
      app_proxy_cleanup_singbox_environment
      app_proxy_cleanup_known_official_proxy_configs
      app_proxy_uninstall_data_dir
      ;;
    2)
      app_proxy_cleanup_singbox_environment
      app_proxy_cleanup_known_official_proxy_configs
      ;;
    3)
      app_proxy_prompt_for_proxy_cleanup 1
      ;;
    *)
      echo "invalid cleanup mode: $mode" >&2
      return 1
      ;;
  esac
}
