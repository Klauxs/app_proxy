# App Proxy Installer

This installer creates per-app proxy wrappers on macOS. It is separate from the Claude Proxy Wrapper package.

User-facing scripts:

- `install-app-proxy.command`
- `uninstall-app-proxy.command`
- `manage-app-proxy.command` (double-click front end for the `app-proxy` CLI)

## app-proxy CLI and manifest

The installer also installs an `app-proxy` CLI (symlinked into the Homebrew bin directory) and a
manifest at `~/Library/Application Support/App Proxy/singbox-manifest.json` (mode 600). The manifest
is the single source of truth: the sing-box config is fully regenerated from it on every change
(`sing-box check` runs before restart; manual edits to config.json are overwritten). Capabilities:

- `app-proxy` — interactive menu (shows pending warnings first)
- `app-proxy status` — profiles, ports, selected nodes, Claude rules target, wrapper bindings
- `app-proxy port add` — add another local HTTP inbound with its own egress. Sources: a
  subscription URL, or a manual `http` (optional TLS) / `socks5` node (server, port, optional
  username/password; paste `http://user:pass@host:port` style URIs directly). Manual nodes can
  optionally detour through an existing profile's egress group, or through a new subscription
  added on the spot (which becomes a normal port profile). The relay carrier must NOT use XTLS
  Vision flow or REALITY (`flow=xtls-rprx-vision`, `security=reality`): Vision splices the byte
  stream and REALITY expects a specific handshake, so a proxy connection tunneled through such a
  node gets corrupted and times out. Relay carriers must be anytls/trojan/ss/plain vless — the
  relay picker warns and the generator refuses rather than emitting a silently-broken detour.
  Pasted proxy URIs keep credentials literal (no percent-decoding): real providers issue
  passwords that may themselves contain `%XX`, and decoding would corrupt them.
- `app-proxy node` — toggle egress group membership, edit manual node fields, update from
  subscription, or delete a profile. Subscription node fields are not editable; they follow the
  subscription on update.
- `app-proxy update [id|--all] [--non-interactive]` — refresh subscriptions. Nodes are aligned by
  name; vanished selected nodes are dropped with a diff summary. If an update would empty an
  egress group, interactive runs re-open node selection; non-interactive runs leave everything
  unchanged and record a pending warning. A launchd agent (`com.app-proxy.refresh`) runs the
  non-interactive update every 24 hours.
- `app-proxy bind` / `app-proxy port delete <id>` — rebind an existing Proxy app to another port
  (official Claude/Codex configs are synced, Claude domain rules migrate), or delete a port.
  Deleting is blocked while wrappers, relay nodes, or Claude rules depend on the port.

Each inbound routes to its own egress group (`pfN-in` → `pfN-auto`). Claude domain rules stay
global and point at the egress of the profile the Claude wrapper is bound to. When a sing-box
config exists but no manifest does, `app-proxy` offers to import it (imported nodes cannot be
subscription-updated) before managing anything.

## App clones (isolated instances)

`app-proxy clone add` creates an isolated instance of a Chromium/Electron app (Claude and Codex
desktop are the primary targets; the mechanism is generic). Every clone redirects `HOME` to
`~/Library/Application Support/App Proxy/Clones/<name>/home` and launches the app with
`--user-data-dir` pointing inside that home — the Chromium profile (and the single-instance
lock it contains) is resolved from the real user home, not `$HOME`, so the explicit flag is
what actually separates app data, and `~/.claude`/`~/.codex` are separated by the `HOME`
redirect. Main app and clones never share state. Two clone types:

- **Full clone (shadow bundle, recommended)** — runs ALONGSIDE the original app. The clone
  bundle carries its own Launch Services identity: the whole app is copied via APFS clonefile
  (instant, copy-on-write, near-zero disk) because helper-process sandboxes refuse to load
  frameworks through symlinks into the original bundle. The Info.plist identity keys are
  rewritten (integrity keys like ElectronAsarIntegrity are retained) and the bundle is re-signed
  ad hoc without the hardened runtime — natively signed apps like Codex carry
  provisioning-backed entitlements that would otherwise make AMFI kill the modified bundle at
  exec. The launcher self-heals after the original app updates (re-copies the payload,
  refreshes Info.plist, and re-signs on next launch). TCC permissions (microphone etc.) are
  granted per clone on first use.
- **Lightweight clone** — a zero-copy launcher that executes the original app's binary directly.
  Cheapest, updates apply instantly, but the running process occupies the original app's Launch
  Services identity, so the clone shares the Dock/app-switcher identity with the original app.

- Initial data is a blank profile by default; optionally copy the main app's current data.
- Each clone either connects directly (ambient proxy env is stripped) or binds to a port profile;
  official Claude/Codex proxy config is written inside the clone's home only.
- Proxied clones get their own guard launch agent: if the app relaunches itself inside the
  clone's home without the proxy flag, the guard terminates it and relaunches through the clone
  wrapper. Guards are clone-aware — the main wrapper's guard never touches clone-owned processes
  (it identifies them by the process `HOME` under `App Proxy/Clones/`), and each clone guard only
  manages processes inside its own home. Direct clones have nothing to enforce and their guard
  exits idle.
- `clone rename` keeps the data home; `clone delete` keeps the data home unless `--purge` and
  removes the clone's guard agent.

## Global settings

DNS (DoH) and the upstream interface are sing-box instance-wide, not per port.
`app-proxy settings doh <url|default>` and `app-proxy settings interface <name|auto>` change
them and regenerate the config; `app-proxy settings` opens the interactive menu. The first
`port add` on a manifest without a configured DoH server offers the choice once.

## Health check

`app-proxy test [profile...]` verifies each port end to end: DNS servers from the live sing-box
config, inbound listening, exit IP through the port (`https://ifconfig.me`), and an HTTP
connectivity probe (`https://www.gstatic.com/generate_204`). Non-zero exit if anything fails.

## Uninstall

Modes: (1) Clean all — automatically sweeps every managed wrapper and clone (clone data homes
are kept unless you opt in to deleting them), then removes the sing-box environment, manifest,
CLI, and refresh agent; (2) environment only; (3) one selected wrapper or clone, picked from a
numbered list or by dragging the .app. `app-proxy uninstall` runs the same flow without the DMG.

It detects or configures a sing-box HTTP inbound, then creates one `AppName Proxy.app` for one dragged `.app` per install run. The runtime config is written to `~/Library/Application Support/App Proxy/AppName Proxy/config.env`; because this path contains spaces, open it from Terminal with quotes, for example `open "$HOME/Library/Application Support/App Proxy/Claude Proxy/config.env"`.

When the dragged app is Claude, the wrapper launches the app with proxy environment variables and `--proxy-server`, and the installer also updates `~/.claude/settings.json` with proxy environment variables under `env` as extra insurance. Claude Desktop has no confirmed public official app-level proxy config file; this settings write is intentionally conservative. When the dragged app is Codex, the installer updates both `~/.codex/.env` and `~/.codex/config.toml` under `[shell_environment_policy.set]`. Existing files are backed up before modification. If existing proxy values differ, the installer asks before overwriting them; declining stops the install before creating the Proxy app.

## sing-box setup

If a usable sing-box HTTP proxy is already running, the installer reports the local port and exit IP, then skips proxy configuration. A local listening port alone is not considered usable; traffic through the HTTP proxy must also pass the egress IP test.

If sing-box is not available, the installer can install Homebrew and sing-box automatically. Homebrew may ask for the current user's administrator password. If the current account cannot use administrator privileges, install and start sing-box manually or rerun this installer from an administrator account.

During generated sing-box setup, the installer tries to bind outbound node connections to an active Wi-Fi interface first, then an active physical interface. Each automatic choice must pass a connectivity test. After writing the config, the installer restarts sing-box and verifies the local HTTP inbound. If an auto-selected interface fails at that stage, it regenerates the config with `Auto binding`, which writes sing-box `route.auto_detect_interface = true`; you can still choose a specific interface manually from the fallback TUI.

Generated sing-box configs are IPv4-only by default. DNS uses `strategy = ipv4_only`; DoH bootstrap and node domain lookups use the sing-box 1.12+ `domain_resolver = { server, strategy = ipv4_only }` form instead of the deprecated `domain_strategy` field. This avoids broken IPv6 paths in virtual machines without changing macOS system IPv6 settings.

DoH configuration is written without live probing. The installer always includes Google DoH, Cloudflare DoH, and AliDNS DoH. If you enter a custom DoH server, it is written as the primary `doh` server before the built-ins; otherwise Google is primary. AliDNS is included as the last built-in DoH server.

After sing-box starts, the installer tests the proxy egress through the local HTTP inbound. If the endpoint listens but cannot reach the internet through the selected node, installation stops before asking for the target app.

When the dragged app is Claude, the installer also prepends sing-box route rules for Anthropic/Claude domains and keywords to the current sing-box config. These rules always route matching traffic to the installer-managed `proxy-auto` outbound, which points at the subscription nodes selected during this installer flow. If the current sing-box config does not contain that managed `proxy-auto` outbound, Claude installation stops instead of routing Claude domains through `route.final`, `direct`, or an arbitrary existing outbound. These rules only affect traffic that enters sing-box; they do not replace the wrapper/guard path and they are not a TUN-based system-wide capture mode.

## Subscription input

Enter a subscription URL. The installer tries common client user agents including Loon, Quantumult X, Surge, Shadowrocket, and Clash. The downloaded subscription may be Clash YAML, Loon/Surge/Shadowrocket style proxy config, Quantumult X server config, plain text node URIs, or base64 encoded node URIs.

Supported node types inside the subscription content: anytls, vless, vmess, ss, trojan, and hysteria2.

Nodes are grouped by country using the node name and flag icon when available.

## Failure behavior

If the configured proxy port is not listening, the generated Proxy app still launches the target app with `--proxy-server=http://127.0.0.1:<port>`.

## Uninstall boundary

The uninstaller offers cleanup modes for the sing-box environment, one selected Proxy app and its guard, or both. Homebrew itself is preserved. When uninstalling a selected Claude or Codex Proxy app, it asks whether to remove matching official config proxy entries. Only entries matching that Proxy app's `http://127.0.0.1:<port>` value are removed.
