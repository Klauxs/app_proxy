// Migrated from supplied macOS JXA. See scripts/migrate-legacy.ts.
function countryCode(country) {
  var value = String(country || '').toLowerCase();
  if (value === 'hong kong') {
    return 'hk';
  }
  if (value === 'taiwan') {
    return 'tw';
  }
  if (value === 'japan') {
    return 'jp';
  }
  if (value === 'us' || value === 'usa' || value === 'united states') {
    return 'us';
  }
  return '';
}

function slugPart(value) {
  return String(value || '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
}

function validatePort(value, fieldName) {
  var stringValue;
  if (typeof value === 'number') {
    if (!isFinite(value) || Math.floor(value) !== value) {
      throw new Error(fieldName + ' must be a decimal integer');
    }
    stringValue = String(value);
  } else {
    stringValue = String(value || '');
    if (!/^[0-9]+$/.test(stringValue)) {
      throw new Error(fieldName + ' must be a decimal integer');
    }
  }

  var port = Number(stringValue);
  if (port < 1 || port > 65535) {
    throw new Error(fieldName + ' must be between 1 and 65535');
  }
  return port;
}

function tagForNode(node, index, usedTags) {
  var parts = ['node'];
  var country = countryCode(node.country);
  if (country) {
    parts.push(country);
  }
  parts.push(node.protocol === 'ss' ? 'ss' : slugPart(node.protocol || 'proxy'));

  var numberMatch = String(node.name || '').match(/(?:^|\D)(\d{1,3})(?:\D*)$/);
  parts.push(numberMatch ? numberMatch[1] : String(index + 1));

  var tag = parts.join('-');
  var base = tag;
  var suffix = 2;
  while (usedTags[tag]) {
    tag = base + '-' + suffix;
    suffix += 1;
  }
  usedTags[tag] = true;
  return tag;
}

function queryValue(node, key) {
  return node && node.query ? node.query[key] : undefined;
}

function boolish(value) {
  var normalized = String(value || '').toLowerCase();
  return value === true || normalized === 'true' || normalized === '1' || normalized === 'yes' || normalized === 'tls';
}

function queryFirst(node, keys) {
  var index;
  var value;
  for (index = 0; index < keys.length; index++) {
    value = queryValue(node, keys[index]);
    if (value !== undefined && value !== null && value !== '') {
      return value;
    }
  }
  return undefined;
}

function stringList(value) {
  if (Array.isArray(value)) {
    return value.map(function (item) { return String(item); }).filter(Boolean);
  }
  return String(value || '').split(/[|,]/).map(function (item) {
    return item.trim();
  }).filter(Boolean);
}

function numberField(value, fieldName) {
  if (value === undefined || value === null || value === '') {
    return undefined;
  }
  if (!/^[0-9]+$/.test(String(value))) {
    throw new Error(fieldName + ' must be a decimal integer');
  }
  return Number(value);
}

var BUILTIN_DOH_SERVERS = [
  'https://dns.google/dns-query',
  'https://cloudflare-dns.com/dns-query',
  'https://dns.alidns.com/dns-query'
];

function normalizeDohServer(value) {
  var raw = String(value || '').trim();
  var match;
  if (!raw) {
    throw new Error('doh_server must not be empty');
  }
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(raw) && !/^https:\/\//i.test(raw)) {
    throw new Error('doh_server must use https://');
  }
  if (!/^https:\/\//i.test(raw)) {
    raw = 'https://' + raw;
  }
  match = raw.match(/^https:\/\/([^/:?#]+)(?::([0-9]+))?([^?#]*)?(?:[?#].*)?$/i);
  if (!match) {
    throw new Error('doh_server must be an HTTPS URL or host');
  }
  return {
    server: match[1],
    server_port: validatePort(match[2] || 443, 'doh_server port'),
    path: match[3] && match[3] !== '/' ? match[3] : '/dns-query',
    display: 'https://' + match[1] + (match[2] ? ':' + match[2] : '') + (match[3] && match[3] !== '/' ? match[3] : '/dns-query')
  };
}

function dohTagForServer(server, index) {
  if (index === 0) {
    return 'doh';
  }
  if (server.server === 'dns.google') {
    return 'doh-google';
  }
  if (server.server === 'cloudflare-dns.com') {
    return 'doh-cloudflare';
  }
  if (server.server === 'dns.alidns.com') {
    return 'doh-alidns';
  }
  return 'doh-' + (index + 1);
}

function normalizeDohServers(payload) {
  var inputs = [];
  var normalized = [];
  var seen = {};

  if (Array.isArray(payload.doh_servers)) {
    payload.doh_servers.forEach(function (value) {
      if (String(value || '').trim()) {
        inputs.push(value);
      }
    });
  } else if (String(payload.doh_server || '').trim()) {
    inputs.push(payload.doh_server);
  }

  BUILTIN_DOH_SERVERS.forEach(function (value) {
    inputs.push(value);
  });

  inputs.forEach(function (value) {
    var server = normalizeDohServer(value);
    var key = server.server + ':' + server.server_port + server.path;
    if (!seen[key]) {
      server.tag = dohTagForServer(server, normalized.length);
      normalized.push(server);
      seen[key] = true;
    }
  });

  if (!normalized.length) {
    throw new Error('no DoH servers configured');
  }
  return normalized;
}

function normalizeInterface(value) {
  var raw = String(value || '').trim();
  if (!raw) {
    return '';
  }
  if (!/^[A-Za-z0-9._-]+$/.test(raw)) {
    throw new Error('upstream_interface contains invalid characters');
  }
  return raw;
}

function tlsForNode(node, forceEnabled) {
  var security = String(queryValue(node, 'security') || node.tls || '').toLowerCase();
  var sni = queryFirst(node, ['sni', 'serverName', 'servername', 'server_name']) || node.server_name;
  var insecure = queryFirst(node, ['insecure', 'allowInsecure', 'allow_insecure', 'skip-cert-verify', 'skip_cert_verify']);
  var alpn = queryFirst(node, ['alpn']);
  var alpnList = alpn ? stringList(alpn) : [];
  var publicKey = queryFirst(node, ['pbk', 'public-key', 'publicKey', 'public_key']);
  var shortId = queryFirst(node, ['sid', 'short-id', 'shortId', 'short_id']);
  var fingerprint = queryFirst(node, ['fp', 'fingerprint', 'client-fingerprint']);
  var isReality = security === 'reality' || !!publicKey;

  if (!forceEnabled && security !== 'tls' && !isReality && !sni && !insecure && !alpnList.length) {
    return null;
  }
  var tls = { enabled: true };
  if (sni) {
    tls.server_name = sni;
  }
  if (insecure !== undefined) {
    tls.insecure = boolish(insecure);
  }
  if (alpnList.length) {
    tls.alpn = alpnList;
  }
  if (isReality) {
    tls.reality = {
      enabled: true,
      public_key: String(publicKey || '')
    };
    if (shortId !== undefined && shortId !== null) {
      tls.reality.short_id = String(shortId);
    }
    if (!fingerprint) {
      fingerprint = 'chrome';
    }
  }
  if (fingerprint) {
    tls.utls = {
      enabled: true,
      fingerprint: String(fingerprint)
    };
  }
  return tls;
}

function flowForNode(node) {
  var flow = queryFirst(node, ['flow']);
  return flow ? String(flow) : '';
}

function transportForNode(node) {
  var type = String(queryFirst(node, ['type', 'network']) || '').toLowerCase();
  var sni = queryFirst(node, ['sni', 'serverName', 'servername', 'server_name']) || node.server_name;
  if (type === 'ws') {
    var transport = {
      type: 'ws',
      path: String(queryFirst(node, ['path']) || '/')
    };
    var host = queryFirst(node, ['host']) || sni;
    if (host) {
      transport.headers = { Host: String(host) };
    }
    return transport;
  }
  if (type === 'grpc') {
    return {
      type: 'grpc',
      service_name: String(queryFirst(node, ['serviceName', 'service-name', 'service_name', 'path']) || '')
    };
  }
  return null;
}

function anytlsTlsForNode(node) {
  return tlsForNode(node, true);
}

function requireField(node, key) {
  if (node[key] === undefined || node[key] === null || node[key] === '') {
    throw new Error('node missing ' + key);
  }
  return node[key];
}

function relayCarrierIncompatible(node) {
  // XTLS Vision flow splices the byte stream and REALITY expects a specific
  // handshake; a proxy connection tunneled through such a node as a relay
  // carrier gets corrupted. These protocols must be the last hop only.
  var flow = String(queryFirst(node, ['flow']) || '');
  if (/vision/i.test(flow)) {
    return true;
  }
  var security = String(queryValue(node, 'security') || '').toLowerCase();
  if (security === 'reality' || queryFirst(node, ['pbk', 'public-key', 'publicKey', 'public_key'])) {
    return true;
  }
  return false;
}

function optionalCredential(outbound, node) {
  if (String(node.username || '')) {
    outbound.username = String(node.username);
  }
  if (String(node.password || '')) {
    outbound.password = String(node.password);
  }
}

function nodeOutbound(node, tag) {
  var protocol = String(node.protocol || '').toLowerCase();
  var outbound;

  if (protocol === 'http') {
    outbound = {
      type: 'http',
      tag: tag,
      server: requireField(node, 'server'),
      server_port: validatePort(requireField(node, 'server_port'), 'server_port')
    };
    optionalCredential(outbound, node);
    if (node.tls === true || (typeof node.tls === 'string' && boolish(node.tls))) {
      outbound.tls = { enabled: true };
    }
  } else if (protocol === 'socks5') {
    outbound = {
      type: 'socks',
      tag: tag,
      version: '5',
      server: requireField(node, 'server'),
      server_port: validatePort(requireField(node, 'server_port'), 'server_port')
    };
    optionalCredential(outbound, node);
  } else if (protocol === 'ss') {
    outbound = {
      type: 'shadowsocks',
      tag: tag,
      server: requireField(node, 'server'),
      server_port: validatePort(requireField(node, 'server_port'), 'server_port'),
      method: requireField(node, 'method'),
      password: requireField(node, 'password')
    };
  } else if (protocol === 'vless') {
    outbound = {
      type: 'vless',
      tag: tag,
      server: requireField(node, 'server'),
      server_port: validatePort(requireField(node, 'server_port'), 'server_port'),
      uuid: requireField(node, 'uuid')
    };
    var vlessFlow = flowForNode(node);
    if (vlessFlow) {
      outbound.flow = vlessFlow;
    }
    var vlessTls = tlsForNode(node, false);
    if (vlessTls) {
      outbound.tls = vlessTls;
    }
    var vlessTransport = transportForNode(node);
    if (vlessTransport) {
      outbound.transport = vlessTransport;
    }
  } else if (protocol === 'vmess') {
    outbound = {
      type: 'vmess',
      tag: tag,
      server: requireField(node, 'server'),
      server_port: validatePort(requireField(node, 'server_port'), 'server_port'),
      uuid: requireField(node, 'uuid'),
      security: node.security || 'auto',
      alter_id: Number(node.alter_id || 0)
    };
    var vmessTls = tlsForNode(node, false);
    if (vmessTls) {
      outbound.tls = vmessTls;
    }
    var vmessTransport = transportForNode(node);
    if (vmessTransport) {
      outbound.transport = vmessTransport;
    }
  } else if (protocol === 'anytls') {
    outbound = {
      type: 'anytls',
      tag: tag,
      server: requireField(node, 'server'),
      server_port: validatePort(requireField(node, 'server_port'), 'server_port'),
      password: requireField(node, 'password')
    };
    outbound.tls = anytlsTlsForNode(node);
  } else if (protocol === 'trojan') {
    outbound = {
      type: 'trojan',
      tag: tag,
      server: requireField(node, 'server'),
      server_port: validatePort(requireField(node, 'server_port'), 'server_port'),
      password: requireField(node, 'password')
    };
    outbound.tls = tlsForNode(node, true);
    var trojanTransport = transportForNode(node);
    if (trojanTransport) {
      outbound.transport = trojanTransport;
    }
  } else if (protocol === 'hysteria2') {
    outbound = {
      type: 'hysteria2',
      tag: tag,
      server: requireField(node, 'server'),
      server_port: validatePort(requireField(node, 'server_port'), 'server_port'),
      password: requireField(node, 'password'),
      tls: tlsForNode(node, true)
    };
    var upMbps = numberField(queryFirst(node, ['up_mbps', 'upMbps', 'up']), 'up_mbps');
    var downMbps = numberField(queryFirst(node, ['down_mbps', 'downMbps', 'down']), 'down_mbps');
    var obfsType = queryFirst(node, ['obfs', 'obfs_type', 'obfs-type']);
    var obfsPassword = queryFirst(node, ['obfs_password', 'obfsPassword', 'obfs-password']);
    if (upMbps !== undefined) {
      outbound.up_mbps = upMbps;
    }
    if (downMbps !== undefined) {
      outbound.down_mbps = downMbps;
    }
    if (obfsType || obfsPassword) {
      outbound.obfs = {
        type: String(obfsType || 'salamander')
      };
      if (obfsPassword) {
        outbound.obfs.password = String(obfsPassword);
      }
    }
  } else {
    throw new Error('unsupported node protocol: ' + protocol);
  }

  outbound.connect_timeout = '15s';
  outbound.domain_resolver = {
    server: 'doh',
    strategy: 'ipv4_only'
  };
  return outbound;
}

var CLAUDE_DOMAIN_SUFFIXES = [
  'anthropic.com',
  'clau.de',
  'claude.ai',
  'claudeusercontent.com',
  'claude-api.com',
  'claudecontentmoderation.com',
  'claudemcpclient.com'
];

var CLAUDE_DOMAIN_KEYWORDS = ['anthropic', 'claude'];

function urltestGroup(tag, tags) {
  return {
    type: 'urltest',
    tag: tag,
    outbounds: tags,
    url: 'https://www.gstatic.com/generate_204',
    interval: '5m',
    tolerance: 3000,
    idle_timeout: '30m',
    interrupt_exist_connections: false
  };
}

function dnsBlock(dohServers, primaryDohTag) {
  return {
    servers: [
      {
        type: 'local',
        tag: 'local-dns'
      }
    ].concat(dohServers.map(function (dohServer) {
      return {
        type: 'https',
        tag: dohServer.tag,
        server: dohServer.server,
        server_port: dohServer.server_port,
        path: dohServer.path,
        domain_resolver: {
          server: 'local-dns',
          strategy: 'ipv4_only'
        }
      };
    })),
    final: primaryDohTag,
    strategy: 'ipv4_only',
    independent_cache: true
  };
}

function routeBlock(finalTag, primaryDohTag, upstreamInterface) {
  var route = {
    final: 'direct',
    default_domain_resolver: {
      server: primaryDohTag,
      strategy: 'ipv4_only'
    }
  };
  if (upstreamInterface) {
    route.default_interface = upstreamInterface;
  } else {
    route.auto_detect_interface = true;
  }
  return route;
}

function assembleConfig(dohServers, inbounds, outbounds, route) {
  return {
    log: { level: 'info' },
    dns: dnsBlock(dohServers, dohServers[0].tag),
    inbounds: inbounds,
    outbounds: outbounds.concat([{ type: 'direct', tag: 'direct' }]),
    route: route
  };
}

function uniqueTag(base, usedTags) {
  var tag = base;
  var suffix = 2;
  while (usedTags[tag]) {
    tag = base + '-' + suffix;
    suffix += 1;
  }
  usedTags[tag] = true;
  return tag;
}

export function buildManifestConfig(payload) {
  var profiles = payload.profiles;
  if (!profiles.length) {
    throw new Error('empty profiles');
  }
  var settings = payload.settings || {};
  var profilesById = {};
  var seenPorts = {};

  profiles.forEach(function (profile) {
    var id = String(profile && profile.id || '');
    if (!id) {
      throw new Error('profile missing id');
    }
    if (profilesById[id]) {
      throw new Error('duplicate profile id: ' + id);
    }
    profilesById[id] = profile;
    var port = validatePort(profile.listen_port, 'listen_port');
    if (seenPorts[port]) {
      throw new Error('duplicate listen_port: ' + port);
    }
    seenPorts[port] = true;
  });

  var usedTags = {};
  var inbounds = [];
  var groupOutbounds = [];
  var nodeOutbounds = [];
  var inboundRules = [];

  profiles.forEach(function (profile) {
    var id = String(profile.id);
    var selected = (profile.nodes || []).filter(function (node) { return node && node.selected; });
    if (!selected.length) {
      throw new Error('profile ' + id + ' has no selected nodes');
    }
    var tags = selected.map(function (node, index) {
      var tag;
      var outbound;
      if (node.raw_outbound && typeof node.raw_outbound === 'object') {
        tag = uniqueTag(id + '-' + String(node.raw_outbound.tag || ('node-' + (index + 1))), usedTags);
        outbound = JSON.parse(JSON.stringify(node.raw_outbound));
        outbound.tag = tag;
      } else {
        var scratch = {};
        tag = uniqueTag(id + '-' + tagForNode(node, index, scratch), usedTags);
        outbound = nodeOutbound(node, tag);
        if (node.relay_profile) {
          var relayId = String(node.relay_profile);
          if (!profilesById[relayId]) {
            throw new Error('relay_profile references missing profile: ' + relayId);
          }
          if (relayId === id) {
            throw new Error('relay_profile must not reference its own profile: ' + relayId);
          }
          var incompatibleCarrier = (profilesById[relayId].nodes || []).filter(function (carrier) {
            return carrier && carrier.selected && relayCarrierIncompatible(carrier);
          });
          if (incompatibleCarrier.length) {
            throw new Error('relay profile ' + relayId + ' uses XTLS Vision/REALITY node(s) (' +
              incompatibleCarrier.map(function (n) { return String(n.name); }).join(', ') +
              ') which cannot carry a relay; select a non-Vision node (anytls/trojan/ss/plain vless) as the relay egress, or remove the relay');
          }
          outbound.detour = relayId + '-auto';
        }
      }
      nodeOutbounds.push(outbound);
      return tag;
    });

    if (profile.gateway_node) {
      var gateway = profile.gateway_node;
      var gatewayProtocol = String(gateway.protocol || '').toLowerCase();
      if (gatewayProtocol !== 'http' && gatewayProtocol !== 'socks5') {
        throw new Error('profile ' + id + ' gateway_node protocol must be http or socks5');
      }
      var gatewayTag = uniqueTag(id + '-gateway', usedTags);
      nodeOutbounds.push(nodeOutbound(gateway, gatewayTag));
      // first in the urltest group: the LAN gateway wins while reachable
      // (lowest latency, and the high tolerance keeps the pick sticky); when
      // its health check fails the group falls back to the profile nodes,
      // and switches back once the gateway is reachable again
      tags.unshift(gatewayTag);
    }

    inbounds.push({
      type: 'http',
      tag: id + '-in',
      listen: '127.0.0.1',
      listen_port: validatePort(profile.listen_port, 'listen_port')
    });
    groupOutbounds.push(urltestGroup(id + '-auto', tags));
    inboundRules.push({ inbound: [id + '-in'], outbound: id + '-auto' });
  });

  var claudeTarget = String(settings.claude_rules_profile || '');
  var rules = inboundRules;
  // route.final is "block": traffic matching no rule fails loudly instead of
  // silently borrowing another profile's egress.
  var finalTag = 'block';
  if (claudeTarget) {
    if (!profilesById[claudeTarget]) {
      throw new Error('claude_rules_profile references missing profile: ' + claudeTarget);
    }
    // claude rules go AFTER the inbound rules so they only catch traffic that
    // is not already bound to a profile inbound (no cross-port hijacking).
    rules = inboundRules.concat([
      { domain_suffix: CLAUDE_DOMAIN_SUFFIXES, outbound: claudeTarget + '-auto' },
      { domain_keyword: CLAUDE_DOMAIN_KEYWORDS, outbound: claudeTarget + '-auto' }
    ]);
  }

  var dohServers = normalizeDohServers(settings);
  var upstreamInterface = normalizeInterface(settings.upstream_interface);
  var route = routeBlock(finalTag, dohServers[0].tag, upstreamInterface);
  route.rules = rules.map(function(rule) { return Object.assign({action: 'route'}, rule); }).concat([{action: 'reject'}]);

  return assembleConfig(dohServers, inbounds, groupOutbounds.concat(nodeOutbounds), route);
}

function buildLegacyConfig(payload) {
  var nodes = payload.nodes || [];
  if (!Array.isArray(nodes) || nodes.length === 0) {
    throw new Error('empty nodes');
  }

  var usedTags = {};
  var nodeOutbounds = nodes.map(function (node, index) {
    return nodeOutbound(node, tagForNode(node, index, usedTags));
  });
  var tags = nodeOutbounds.map(function (outbound) { return outbound.tag; });
  var dohServers = normalizeDohServers(payload);
  var upstreamInterface = normalizeInterface(payload.upstream_interface);
  var route = routeBlock('proxy-auto', dohServers[0].tag, upstreamInterface);

  var inbounds = [
    {
      type: 'http',
      tag: 'http-in',
      listen: '127.0.0.1',
      listen_port: validatePort(payload.listen_port === undefined ? 18099 : payload.listen_port, 'listen_port')
    }
  ];

  return assembleConfig(dohServers, inbounds, [urltestGroup('proxy-auto', tags)].concat(nodeOutbounds), route);
}
