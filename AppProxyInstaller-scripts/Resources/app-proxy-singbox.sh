#!/bin/bash

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "This V2 sing-box resource is meant to be sourced by install-app-proxy.command." >&2
  exit 64
fi

APP_PROXY_SINGBOX_RESOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if ! declare -F app_proxy_normalize_port >/dev/null 2>&1; then
  . "$APP_PROXY_SINGBOX_RESOURCE_DIR/app-proxy-common.sh"
fi

app_proxy_cmd() {
  local name="$1"
  shift

  if [[ -n "${APP_PROXY_TEST_BIN_DIR:-}" && -x "$APP_PROXY_TEST_BIN_DIR/$name" ]]; then
    "$APP_PROXY_TEST_BIN_DIR/$name" "$@"
    return
  fi

  "$name" "$@"
}

app_proxy_brew_path() {
  local brew_path

  if [[ -n "${APP_PROXY_TEST_BIN_DIR:-}" && -x "$APP_PROXY_TEST_BIN_DIR/brew" ]]; then
    printf '%s\n' "$APP_PROXY_TEST_BIN_DIR/brew"
    return 0
  fi

  brew_path="$(command -v brew 2>/dev/null || true)"
  if [[ -n "$brew_path" ]]; then
    printf '%s\n' "$brew_path"
    return 0
  fi

  if [[ -x /opt/homebrew/bin/brew ]]; then
    printf '%s\n' /opt/homebrew/bin/brew
    return 0
  fi

  if [[ -x /usr/local/bin/brew ]]; then
    printf '%s\n' /usr/local/bin/brew
    return 0
  fi

  return 1
}

app_proxy_brew() {
  local brew_path

  brew_path="$(app_proxy_brew_path)" || return 127
  "$brew_path" "$@"
}

app_proxy_singbox_config_path() {
  local brew_prefix

  if [[ -n "${APP_PROXY_SINGBOX_CONFIG:-}" ]]; then
    printf '%s\n' "$APP_PROXY_SINGBOX_CONFIG"
    return 0
  fi

  brew_prefix="$(app_proxy_brew --prefix 2>/dev/null || true)"
  if [[ -n "$brew_prefix" ]]; then
    printf '%s/etc/sing-box/config.json\n' "$brew_prefix"
    return 0
  fi

  if [[ -x /usr/local/bin/brew || -d /usr/local/Homebrew ]]; then
    printf '%s\n' "/usr/local/etc/sing-box/config.json"
    return 0
  fi

  printf '%s\n' "${APP_PROXY_SINGBOX_CONFIG:-/opt/homebrew/etc/sing-box/config.json}"
}

app_proxy_singbox_http_port() {
  local config="$1"
  local raw_port
  local normalized_port

  if [[ ! -f "$config" ]]; then
    return 0
  fi

  raw_port="$(/usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function run(argv) {
  try {
    var path = argv[0];
    var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
    if (!text) return "";
    var config = JSON.parse(ObjC.unwrap(text));
    var inbounds = Array.isArray(config.inbounds) ? config.inbounds : [];
    for (var i = 0; i < inbounds.length; i++) {
      var inbound = inbounds[i];
      if (inbound && inbound.type === "http" && inbound.listen === "127.0.0.1" && inbound.listen_port !== undefined) {
        return String(inbound.listen_port);
      }
    }
    return "";
  } catch (error) {
    return "";
  }
}
' "$config" 2>/dev/null || true)"

  normalized_port="$(app_proxy_normalize_port "$raw_port" 2>/dev/null || true)"
  if [[ -n "$normalized_port" ]]; then
    printf '%s\n' "$normalized_port"
  fi
}

app_proxy_backup_singbox_config() {
  local config
  local backup

  config="$(app_proxy_singbox_config_path)"
  if [[ ! -f "$config" ]]; then
    printf '\n'
    return 0
  fi

  backup="$(/usr/bin/mktemp "$config.bak-app-proxy-$(/bin/date '+%Y%m%d-%H%M%S').XXXXXX")" || return 1
  if ! /bin/cp "$config" "$backup"; then
    /bin/rm -f "$backup"
    return 1
  fi
  printf '%s\n' "$backup"
}

app_proxy_json_string() {
  local value="$1"

  /usr/bin/osascript -l JavaScript -e '
function run(argv) {
  return JSON.stringify(argv[0]);
}
' "$value"
}

app_proxy_singbox_has_managed_proxy_auto() {
  local config="$1"
  local result

  if [[ ! -f "$config" ]]; then
    return 1
  fi

  result="$(/usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

function run(argv) {
  try {
    var path = argv[0];
    var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
    if (!text) return "false";
    var config = JSON.parse(ObjC.unwrap(text));
    var outbounds = Array.isArray(config.outbounds) ? config.outbounds : [];
    for (var i = 0; i < outbounds.length; i++) {
      var outbound = outbounds[i] || {};
      var managedTag = outbound.tag === "proxy-auto" || /^pf[0-9]+-auto$/.test(String(outbound.tag || ""));
      if (managedTag && Array.isArray(outbound.outbounds) && outbound.outbounds.length > 0) {
        return "true";
      }
    }
    return "false";
  } catch (error) {
    return "false";
  }
}
' "$config" 2>/dev/null || true)"

  [[ "$result" == "true" ]]
}

app_proxy_singbox_claude_rules_outbound() {
  local config="$1"

  if [[ ! -f "$config" ]]; then
    return 0
  fi

  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

var CLAUDE_DOMAIN_SUFFIXES = [
  "anthropic.com",
  "clau.de",
  "claude.ai",
  "claudeusercontent.com",
  "claude-api.com",
  "claudecontentmoderation.com",
  "claudemcpclient.com"
];

var CLAUDE_DOMAIN_KEYWORDS = [
  "anthropic",
  "claude"
];

function arrayValue(value) {
  if (Array.isArray(value)) return value.map(String);
  if (value === undefined || value === null) return [];
  return [String(value)];
}

function sameStringSet(left, right) {
  var a = arrayValue(left).sort();
  var b = arrayValue(right).sort();
  if (a.length !== b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function managedOutboundTag(tag) {
  return tag === "proxy-auto" || /^pf[0-9]+-auto$/.test(String(tag || ""));
}

function run(argv) {
  try {
    var path = argv[0];
    var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
    if (!text) return "";
    var config = JSON.parse(ObjC.unwrap(text));
    var rules = config && config.route && Array.isArray(config.route.rules) ? config.route.rules : [];
    var suffixTag = "";
    var keywordTag = "";
    rules.forEach(function(rule) {
      if (!rule || !managedOutboundTag(String(rule.outbound || ""))) return;
      if (sameStringSet(rule.domain_suffix, CLAUDE_DOMAIN_SUFFIXES)) suffixTag = String(rule.outbound);
      if (sameStringSet(rule.domain_keyword, CLAUDE_DOMAIN_KEYWORDS)) keywordTag = String(rule.outbound);
    });
    return suffixTag && suffixTag === keywordTag ? suffixTag : "";
  } catch (error) {
    return "";
  }
}
' "$config" 2>/dev/null || true
}

app_proxy_singbox_has_claude_proxy_rules() {
  local config="$1"
  local result

  if [[ ! -f "$config" ]]; then
    return 1
  fi

  result="$(/usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

var CLAUDE_DOMAIN_SUFFIXES = [
  "anthropic.com",
  "clau.de",
  "claude.ai",
  "claudeusercontent.com",
  "claude-api.com",
  "claudecontentmoderation.com",
  "claudemcpclient.com"
];

var CLAUDE_DOMAIN_KEYWORDS = [
  "anthropic",
  "claude"
];

function arrayValue(value) {
  if (Array.isArray(value)) return value.map(String);
  if (value === undefined || value === null) return [];
  return [String(value)];
}

function sameStringSet(left, right) {
  var a = arrayValue(left).sort();
  var b = arrayValue(right).sort();
  if (a.length !== b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function run(argv) {
  try {
    var path = argv[0];
    var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
    if (!text) return "false";
    var config = JSON.parse(ObjC.unwrap(text));
    var rules = config && config.route && Array.isArray(config.route.rules) ? config.route.rules : [];
    var hasSuffix = false;
    var hasKeyword = false;
    rules.forEach(function(rule) {
      if (!rule || String(rule.outbound || "") !== "proxy-auto") return;
      if (sameStringSet(rule.domain_suffix, CLAUDE_DOMAIN_SUFFIXES)) hasSuffix = true;
      if (sameStringSet(rule.domain_keyword, CLAUDE_DOMAIN_KEYWORDS)) hasKeyword = true;
    });
    return hasSuffix && hasKeyword ? "true" : "false";
  } catch (error) {
    return "false";
  }
}
' "$config" 2>/dev/null || true)"

  [[ "$result" == "true" ]]
}

app_proxy_patch_singbox_claude_rules() {
  local config="$1"

  if [[ ! -f "$config" ]]; then
    echo "sing-box config missing: $config" >&2
    return 1
  fi

  /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");

var CLAUDE_DOMAIN_SUFFIXES = [
  "anthropic.com",
  "clau.de",
  "claude.ai",
  "claudeusercontent.com",
  "claude-api.com",
  "claudecontentmoderation.com",
  "claudemcpclient.com"
];

var CLAUDE_DOMAIN_KEYWORDS = [
  "anthropic",
  "claude"
];

function readConfig(path) {
  var text = $.NSString.stringWithContentsOfFileEncodingError(path, $.NSUTF8StringEncoding, null);
  if (!text) {
    throw new Error("unable to read config: " + path);
  }
  return JSON.parse(ObjC.unwrap(text));
}

function writeConfig(path, config) {
  var text = JSON.stringify(config, null, 2) + "\n";
  var nsText = $.NSString.alloc.initWithUTF8String(text);
  nsText.writeToFileAtomicallyEncodingError(path, true, $.NSUTF8StringEncoding, null);
}

function arrayValue(value) {
  if (Array.isArray(value)) return value.map(String);
  if (value === undefined || value === null || value === "") return [];
  return [String(value)];
}

function sameStringSet(left, right) {
  var a = arrayValue(left).sort();
  var b = arrayValue(right).sort();
  var i;
  if (a.length !== b.length) return false;
  for (i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function hasManagedProxyAuto(config) {
  var outbounds = Array.isArray(config.outbounds) ? config.outbounds : [];
  var i;
  var outbound;
  for (i = 0; i < outbounds.length; i++) {
    outbound = outbounds[i] || {};
    if (outbound.tag === "proxy-auto" && Array.isArray(outbound.outbounds) && outbound.outbounds.length > 0) {
      return true;
    }
  }
  return false;
}

function isManagedClaudeRule(rule, outboundTag) {
  if (!rule || typeof rule !== "object" || Array.isArray(rule)) return false;
  if (String(rule.outbound || "") !== outboundTag) return false;
  return sameStringSet(rule.domain_suffix, CLAUDE_DOMAIN_SUFFIXES) ||
    sameStringSet(rule.domain_keyword, CLAUDE_DOMAIN_KEYWORDS);
}

function run(argv) {
  var path = argv[0];
  var config = readConfig(path);
  var outboundTag = "proxy-auto";
  var route;
  var rules;
  var filtered;
  if (!config || typeof config !== "object" || Array.isArray(config)) {
    throw new Error("config root must be an object");
  }
  if (!config.route || typeof config.route !== "object" || Array.isArray(config.route)) {
    config.route = {};
  }
  if (!hasManagedProxyAuto(config)) {
    return "__APP_PROXY_NO_MANAGED_PROXY_AUTO__";
  }
  route = config.route;
  rules = Array.isArray(route.rules) ? route.rules : [];
  filtered = rules.filter(function(rule) {
    return !isManagedClaudeRule(rule, outboundTag);
  });
  route.rules = [
    { domain_suffix: CLAUDE_DOMAIN_SUFFIXES, outbound: outboundTag },
    { domain_keyword: CLAUDE_DOMAIN_KEYWORDS, outbound: outboundTag }
  ].concat(filtered);
  writeConfig(path, config);
  return outboundTag;
}
' "$config"
}

app_proxy_singbox_available() {
  if app_proxy_brew list sing-box >/dev/null 2>&1; then
    return 0
  fi
  if [[ -n "${APP_PROXY_TEST_BIN_DIR:-}" && -x "$APP_PROXY_TEST_BIN_DIR/sing-box" ]]; then
    return 0
  fi
  command -v sing-box >/dev/null 2>&1
}

app_proxy_singbox_binary_path() {
  local brew_prefix
  local command_path

  if [[ -n "${APP_PROXY_TEST_BIN_DIR:-}" && -x "$APP_PROXY_TEST_BIN_DIR/sing-box" ]]; then
    printf '%s\n' "$APP_PROXY_TEST_BIN_DIR/sing-box"
    return 0
  fi

  command_path="$(command -v sing-box 2>/dev/null || true)"
  if [[ -n "$command_path" ]]; then
    printf '%s\n' "$command_path"
    return 0
  fi

  brew_prefix="$(app_proxy_brew --prefix 2>/dev/null || true)"
  if [[ -n "$brew_prefix" && -x "$brew_prefix/bin/sing-box" ]]; then
    printf '%s/bin/sing-box\n' "$brew_prefix"
    return 0
  fi

  if [[ -x /opt/homebrew/bin/sing-box ]]; then
    printf '%s\n' /opt/homebrew/bin/sing-box
    return 0
  fi
  if [[ -x /usr/local/bin/sing-box ]]; then
    printf '%s\n' /usr/local/bin/sing-box
    return 0
  fi

  return 1
}

app_proxy_check_singbox_config() {
  local config
  local singbox_path

  config="$(app_proxy_singbox_config_path)"
  singbox_path="$(app_proxy_singbox_binary_path)" || {
    echo "Warning: sing-box binary not found; skipping config check." >&2
    return 0
  }

  "$singbox_path" check -c "$config"
}

app_proxy_singbox_state_json() {
  local config
  local port
  local installed=false
  local running=false
  local usable=false
  local exit_ip=""
  local exit_ip_json
  local config_json

  config="$(app_proxy_singbox_config_path)"
  port="$(app_proxy_singbox_http_port "$config")"

  if app_proxy_singbox_available; then
    installed=true
  fi

  if [[ -n "$port" ]] && app_proxy_cmd nc -z 127.0.0.1 "$port" >/dev/null 2>&1; then
    running=true
    exit_ip="$(app_proxy_cmd curl -x "http://127.0.0.1:$port" -fsS --max-time 20 https://ifconfig.me 2>/dev/null || true)"
    exit_ip="${exit_ip//$'\n'/}"
    exit_ip="${exit_ip//$'\r'/}"
    if [[ -n "$exit_ip" ]]; then
      usable=true
    fi
  fi

  exit_ip_json="$(app_proxy_json_string "$exit_ip")"
  config_json="$(app_proxy_json_string "$config")"

  if [[ -n "$port" ]]; then
    printf '{"installed":%s,"running":%s,"usable":%s,"listen_port":%s,"exit_ip":%s,"config":%s}\n' \
      "$installed" "$running" "$usable" "$port" "$exit_ip_json" "$config_json"
  else
    printf '{"installed":%s,"running":%s,"usable":%s,"listen_port":null,"exit_ip":%s,"config":%s}\n' \
      "$installed" "$running" "$usable" "$exit_ip_json" "$config_json"
  fi
}

app_proxy_write_singbox_config() {
  local generated_config="$1"
  local config
  local config_dir
  local backup_path
  local temp_config

  if [[ ! -f "$generated_config" ]]; then
    echo "generated sing-box config missing: $generated_config" >&2
    return 1
  fi

  config="$(app_proxy_singbox_config_path)"
  config_dir="$(/usr/bin/dirname "$config")"
  if [[ -f "$config" ]]; then
    backup_path="$(app_proxy_backup_singbox_config)" || return 1
    if [[ -z "$backup_path" ]]; then
      return 1
    fi
  fi
  /bin/mkdir -p "$config_dir" || return 1
  temp_config="$(/usr/bin/mktemp "$config.tmp-app-proxy.XXXXXX")" || return 1
  if ! /bin/cp "$generated_config" "$temp_config"; then
    /bin/rm -f "$temp_config"
    return 1
  fi
  if ! /bin/mv "$temp_config" "$config"; then
    /bin/rm -f "$temp_config"
    return 1
  fi
}

app_proxy_restart_singbox() {
  app_proxy_brew services restart sing-box
}
