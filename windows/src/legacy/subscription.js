// Migrated from supplied macOS JXA. See scripts/migrate-legacy.ts.
function b64decode(value) {
  const normalized = String(value || '').replace(/\s+/g, '').replace(/-/g, '+').replace(/_/g, '/');
  if (!normalized || !/^[A-Za-z0-9+/]*={0,2}$/.test(normalized)) return null;
  try { return new TextDecoder('utf-8', { fatal: true }).decode(Buffer.from(normalized, 'base64')); }
  catch { return null; }
}
function safeDecode(value) {
  try {
    return decodeURIComponent(String(value || '').replace(/\+/g, '%20'));
  } catch (error) {
    return String(value || '');
  }
}

function charFromCodePoint(codePoint) {
  if (codePoint <= 0xffff) {
    return String.fromCharCode(codePoint);
  }
  codePoint -= 0x10000;
  return String.fromCharCode(0xd800 + (codePoint >> 10), 0xdc00 + (codePoint & 0x3ff));
}

function decodeUnicodeEscapes(value) {
  return String(value || '')
    .replace(/\\U([0-9a-fA-F]{8})/g, function (_, hex) {
      return charFromCodePoint(parseInt(hex, 16));
    })
    .replace(/\\u\{([0-9a-fA-F]{1,6})\}/g, function (_, hex) {
      return charFromCodePoint(parseInt(hex, 16));
    })
    .replace(/\\u([0-9a-fA-F]{4})/g, function (_, hex) {
      return String.fromCharCode(parseInt(hex, 16));
    });
}

function displayText(value) {
  return decodeUnicodeEscapes(value);
}

function countryForName(name) {
  var raw = displayText(name);
  var value = raw.toLowerCase();

  if (raw.indexOf('🇹🇼') >= 0 || /(^|[^a-z0-9])(taiwan|tw)([^a-z0-9]|$)|台灣|台湾/.test(value)) {
    return 'Taiwan';
  }
  if (raw.indexOf('🇭🇰') >= 0 || /(^|[^a-z0-9])(hong\s*kong|hk)([^a-z0-9]|$)|香港/.test(value)) {
    return 'Hong Kong';
  }
  if (raw.indexOf('🇯🇵') >= 0 || /(^|[^a-z0-9])(japan|jp)([^a-z0-9]|$)|日本|东京|東京/.test(value)) {
    return 'Japan';
  }
  if (raw.indexOf('🇺🇸') >= 0 || /\b(us|usa|united states|america)\b|美国|美國/.test(value)) {
    return 'US';
  }
  if (raw.indexOf('🇸🇬') >= 0 || /\b(singapore|sg)\b|新加坡|狮城|獅城/.test(value)) {
    return 'Singapore';
  }
  if (raw.indexOf('🇰🇷') >= 0 || /\b(korea|kr|south korea)\b|韩国|韓國|首尔|首爾/.test(value)) {
    return 'South Korea';
  }
  if (raw.indexOf('🇬🇧') >= 0 || /\b(uk|united kingdom|britain|england)\b|英国|英國/.test(value)) {
    return 'UK';
  }
  if (raw.indexOf('🇩🇪') >= 0 || /\b(germany|de)\b|德国|德國/.test(value)) {
    return 'Germany';
  }
  if (raw.indexOf('🇫🇷') >= 0 || /\b(france|fr)\b|法国|法國/.test(value)) {
    return 'France';
  }
  if (raw.indexOf('🇳🇱') >= 0 || /\b(netherlands|nl|holland)\b|荷兰|荷蘭/.test(value)) {
    return 'Netherlands';
  }
  if (raw.indexOf('🇨🇦') >= 0 || /\b(canada|ca)\b|加拿大/.test(value)) {
    return 'Canada';
  }
  if (raw.indexOf('🇦🇺') >= 0 || /\b(australia|au)\b|澳大利亚|澳大利亞|澳洲/.test(value)) {
    return 'Australia';
  }
  return 'Unknown';
}

function parseQuery(queryString) {
  var query = {};
  if (!queryString) {
    return query;
  }

  queryString.split('&').forEach(function (pair) {
    if (!pair) {
      return;
    }
    var equals = pair.indexOf('=');
    var key = equals >= 0 ? pair.slice(0, equals) : pair;
    var value = equals >= 0 ? pair.slice(equals + 1) : '';
    query[safeDecode(key)] = safeDecode(value);
  });
  return query;
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

function parseServerPort(hostPort) {
  var lastColon = String(hostPort || '').lastIndexOf(':');
  if (lastColon < 0) {
    throw new Error('missing port');
  }
  var server = hostPort.slice(0, lastColon);
  if (!server) {
    throw new Error('missing server');
  }
  return { server: server, server_port: validatePort(hostPort.slice(lastColon + 1), 'server_port') };
}

function parseStandard(source, protocol, scheme) {
  var schemeName = scheme || protocol;
  var withoutScheme = source.slice(schemeName.length + 3);
  var hashIndex = withoutScheme.indexOf('#');
  var fragment = hashIndex >= 0 ? withoutScheme.slice(hashIndex + 1) : '';
  var beforeFragment = hashIndex >= 0 ? withoutScheme.slice(0, hashIndex) : withoutScheme;
  var queryIndex = beforeFragment.indexOf('?');
  var queryString = queryIndex >= 0 ? beforeFragment.slice(queryIndex + 1) : '';
  var authority = queryIndex >= 0 ? beforeFragment.slice(0, queryIndex) : beforeFragment;
  var atIndex = authority.lastIndexOf('@');
  var userinfo = atIndex >= 0 ? authority.slice(0, atIndex) : '';
  var hostPort = atIndex >= 0 ? authority.slice(atIndex + 1) : authority;
  var endpoint = parseServerPort(hostPort);
  var name = displayText(safeDecode(fragment) || endpoint.server);

  var node = {
    status: 'supported',
    protocol: protocol,
    name: name,
    country: countryForName(name),
    server: endpoint.server,
    server_port: endpoint.server_port,
    query: parseQuery(queryString)
  };

  if (protocol === 'anytls' || protocol === 'trojan' || protocol === 'hysteria2') {
    if (!userinfo) {
      throw new Error(protocol + ' password is required');
    }
    node.password = safeDecode(userinfo);
  } else if (userinfo) {
    node.uuid = safeDecode(userinfo);
  }
  return node;
}

function parseVmess(source) {
  var payload = source.slice('vmess://'.length);
  var decoded = b64decode(payload);
  if (!decoded) {
    throw new Error('invalid vmess base64');
  }

  var data = JSON.parse(decoded);
  var name = displayText(data.ps || data.name || data.add || 'VMess');
  var node = {
    status: 'supported',
    protocol: 'vmess',
    name: name,
    country: countryForName(name),
    server: data.add,
    server_port: validatePort(data.port, 'server_port'),
    uuid: data.id
  };

  if (data.aid !== undefined && data.aid !== '') {
    node.alter_id = parseInt(data.aid, 10) || 0;
  }
  if (data.scy) {
    node.security = data.scy;
  }
  node.query = {};
  if (data.net) {
    node.query.type = data.net;
  }
  if (data.tls) {
    node.query.security = data.tls;
  }
  if (data.sni) {
    node.query.sni = data.sni;
  }
  if (data.host) {
    node.query.host = data.host;
  }
  if (data.path) {
    node.query.path = data.path;
  }
  return node;
}

function parseShadowsocks(source) {
  var withoutScheme = source.slice('ss://'.length);
  var hashIndex = withoutScheme.indexOf('#');
  var fragment = hashIndex >= 0 ? withoutScheme.slice(hashIndex + 1) : '';
  var beforeFragment = hashIndex >= 0 ? withoutScheme.slice(0, hashIndex) : withoutScheme;
  var name = displayText(safeDecode(fragment) || 'Shadowsocks');
  var atIndex = beforeFragment.lastIndexOf('@');
  var userinfo;
  var hostPort;

  if (atIndex >= 0) {
    userinfo = beforeFragment.slice(0, atIndex);
    hostPort = beforeFragment.slice(atIndex + 1);
    if (userinfo.indexOf(':') < 0) {
      userinfo = b64decode(userinfo) || userinfo;
    } else {
      userinfo = safeDecode(userinfo);
    }
  } else {
    var decoded = b64decode(beforeFragment);
    if (!decoded) {
      throw new Error('invalid shadowsocks base64');
    }
    atIndex = decoded.lastIndexOf('@');
    if (atIndex < 0) {
      throw new Error('missing shadowsocks endpoint');
    }
    userinfo = decoded.slice(0, atIndex);
    hostPort = decoded.slice(atIndex + 1);
  }

  var colon = userinfo.indexOf(':');
  if (colon < 0) {
    throw new Error('missing shadowsocks method or password');
  }
  var endpoint = parseServerPort(hostPort);
  return {
    status: 'supported',
    protocol: 'ss',
    name: name,
    country: countryForName(name),
    server: endpoint.server,
    server_port: endpoint.server_port,
    method: userinfo.slice(0, colon),
    password: userinfo.slice(colon + 1)
  };
}

function parseUri(source, sourceIndex) {
  var schemeMatch = String(source).match(/^([A-Za-z][A-Za-z0-9+.-]*):\/\//);
  if (!schemeMatch) {
    return {
      status: 'unsupported',
      protocol: 'unknown',
      reason: 'missing protocol',
      source_index: sourceIndex
    };
  }

  var protocol = schemeMatch[1].toLowerCase();
  try {
    if (protocol === 'vmess') {
      return parseVmess(source);
    }
    if (protocol === 'ss') {
      return parseShadowsocks(source);
    }
    if (protocol === 'vless' || protocol === 'anytls' || protocol === 'trojan') {
      return parseStandard(source, protocol);
    }
    if (protocol === 'hysteria2' || protocol === 'hy2') {
      return parseStandard(source, 'hysteria2', protocol);
    }
    return {
      status: 'unsupported',
      protocol: protocol,
      reason: 'unsupported protocol',
      source_index: sourceIndex
    };
  } catch (error) {
    return {
      status: 'unsupported',
      protocol: protocol,
      reason: String(error.message || error),
      source_index: sourceIndex
    };
  }
}

function yamlIndent(line) {
  var match = String(line || '').match(/^ */);
  return match ? match[0].length : 0;
}

function unquoteYamlScalar(value) {
  var raw = String(value === undefined || value === null ? '' : value).trim();
  var first = raw.charAt(0);
  var last = raw.charAt(raw.length - 1);

  if ((first === '"' && last === '"') || (first === "'" && last === "'")) {
    raw = raw.slice(1, -1);
    if (first === '"') {
      raw = raw.replace(/\\"/g, '"').replace(/\\n/g, '\n').replace(/\\\\/g, '\\');
    } else {
      raw = raw.replace(/''/g, "'");
    }
  }

  if (/^(true|false)$/i.test(raw)) {
    return /^true$/i.test(raw);
  }
  if (/^[0-9]+$/.test(raw)) {
    return Number(raw);
  }
  return displayText(raw);
}

function splitYamlInlineMap(value) {
  var text = String(value || '').trim();
  var result = [];
  var current = '';
  var quote = '';
  var index;
  var char;

  if (text.charAt(0) === '{' && text.charAt(text.length - 1) === '}') {
    text = text.slice(1, -1);
  }

  for (index = 0; index < text.length; index++) {
    char = text.charAt(index);
    if (quote) {
      current += char;
      if (char === quote && text.charAt(index - 1) !== '\\') {
        quote = '';
      }
      continue;
    }
    if (char === '"' || char === "'") {
      quote = char;
      current += char;
      continue;
    }
    if (char === ',') {
      if (current.trim()) {
        result.push(current.trim());
      }
      current = '';
      continue;
    }
    current += char;
  }
  if (current.trim()) {
    result.push(current.trim());
  }
  return result;
}

function assignYamlKeyValue(target, source) {
  var index = String(source || '').indexOf(':');
  var key;
  var value;

  if (index < 0) {
    return;
  }
  key = String(source).slice(0, index).trim();
  value = String(source).slice(index + 1).trim();
  key = String(unquoteYamlScalar(key));
  target[key] = unquoteYamlScalar(value);
}

function parseYamlInlineMap(source) {
  var result = {};
  splitYamlInlineMap(source).forEach(function (entry) {
    assignYamlKeyValue(result, entry);
  });
  return result;
}

function protocolAlias(value) {
  var protocol = String(value || '').toLowerCase();
  if (protocol === 'shadowsocks') {
    return 'ss';
  }
  if (protocol === 'hy2' || protocol === 'hysteria-2') {
    return 'hysteria2';
  }
  if (protocol === 'ss' || protocol === 'vless' || protocol === 'vmess' ||
      protocol === 'anytls' || protocol === 'trojan' || protocol === 'hysteria2') {
    return protocol;
  }
  return '';
}

function boolish(value) {
  var normalized = String(value || '').toLowerCase();
  return value === true || normalized === 'true' || normalized === '1' || normalized === 'yes' || normalized === 'tls';
}

function firstPresent(source, names) {
  var index;
  var key;
  var canonicalKey;
  for (index = 0; index < names.length; index++) {
    key = names[index];
    if (source[key] !== undefined && source[key] !== '') {
      return source[key];
    }
    canonicalKey = canonicalConfigKey(key);
    if (source[canonicalKey] !== undefined && source[canonicalKey] !== '') {
      return source[canonicalKey];
    }
  }
  return undefined;
}

function copyIfPresent(target, targetKey, source, sourceKeys) {
  var value = firstPresent(source, sourceKeys);
  if (value !== undefined && value !== '') {
    target[targetKey] = value;
  }
}

function tlsQueryFromFields(source) {
  var query = {};
  copyIfPresent(query, 'sni', source, ['sni', 'servername', 'server-name', 'serverName', 'tls-name', 'tlsName', 'peer']);
  copyIfPresent(query, 'alpn', source, ['alpn']);
  copyIfPresent(query, 'obfs', source, ['obfs', 'obfs-type', 'obfs_type']);
  copyIfPresent(query, 'obfs_password', source, ['obfs-password', 'obfs_password', 'obfsPassword']);
  copyIfPresent(query, 'up_mbps', source, ['up', 'up-mbps', 'up_mbps', 'upMbps', 'upmbps']);
  copyIfPresent(query, 'down_mbps', source, ['down', 'down-mbps', 'down_mbps', 'downMbps', 'downmbps']);
  copyIfPresent(query, 'server_ports', source, ['ports', 'server-ports', 'server_ports', 'serverPorts']);

  if (boolish(firstPresent(source, ['tls', 'security', 'over-tls', 'over_tls', 'overTls']))) {
    query.security = 'tls';
  }
  if (boolish(firstPresent(source, ['skip-cert-verify', 'skip_cert_verify', 'allowInsecure', 'allow-insecure', 'insecure']))) {
    query.insecure = true;
  }
  return query;
}

function mergeQuery(target, source) {
  Object.keys(source || {}).forEach(function (key) {
    if (source[key] !== undefined && source[key] !== '') {
      target[key] = source[key];
    }
  });
}

function clashNodeFromProxy(proxy, sourceIndex) {
  var rawType = String(proxy.type || '').toLowerCase();
  var type = protocolAlias(rawType);
  var name = displayText(proxy.name || proxy.server || 'node');
  var node;

  if (!type) {
    return {
      status: 'unsupported',
      protocol: rawType || 'unknown',
      reason: 'unsupported Clash proxy type',
      source_index: sourceIndex
    };
  }

  try {
    node = {
      status: 'supported',
      protocol: type,
      name: name,
      country: countryForName(name),
      server: proxy.server,
      server_port: validatePort(proxy.port || proxy.server_port, 'server_port'),
      query: tlsQueryFromFields(proxy)
    };

    if (node.protocol === 'ss') {
      node.method = proxy.cipher || proxy.method;
      node.password = proxy.password;
      if (!node.method || !node.password) {
        throw new Error('missing shadowsocks cipher or password');
      }
      delete node.query;
      return node;
    }

    if (node.protocol === 'vless' || node.protocol === 'vmess') {
      node.uuid = proxy.uuid || proxy.id;
      if (!node.uuid) {
        throw new Error('missing uuid');
      }
      if (proxy.network) {
        node.query.type = proxy.network;
      }
      if (proxy.host) {
        node.query.host = proxy.host;
      }
      if (proxy.path) {
        node.query.path = proxy.path;
      }
      if (node.protocol === 'vmess') {
        node.security = proxy.cipher || proxy.security || 'auto';
        node.alter_id = Number(proxy.alterId || proxy['alter-id'] || proxy.alter_id || 0);
      }
      return node;
    }

    if (node.protocol === 'anytls' || node.protocol === 'trojan' || node.protocol === 'hysteria2') {
      node.password = proxy.password || proxy.auth || proxy['auth-str'] || proxy.auth_str;
      if (!node.password) {
        throw new Error('missing password');
      }
      return node;
    }
  } catch (error) {
    return {
      status: 'unsupported',
      protocol: type || rawType || 'unknown',
      reason: String(error.message || error),
      source_index: sourceIndex
    };
  }
}

function parseClashYaml(input) {
  var lines = String(input || '').split(/\r?\n/);
  var proxiesIndex = -1;
  var proxiesIndent = 0;
  var proxies = [];
  var nodeIndent = null;
  var listKey = null;
  var listIndent = 0;
  var current = null;
  var index;
  var line;
  var trimmed;
  var rest;

  for (index = 0; index < lines.length; index++) {
    if (/^proxies:\s*(?:#.*)?$/.test(lines[index])) {
      proxiesIndex = index;
      proxiesIndent = yamlIndent(lines[index]);
      break;
    }
  }
  if (proxiesIndex < 0) {
    return null;
  }

  for (index = proxiesIndex + 1; index < lines.length; index++) {
    line = lines[index];
    trimmed = line.trim();
    if (!trimmed || trimmed.charAt(0) === '#') {
      continue;
    }
    if (yamlIndent(line) <= proxiesIndent && /^[A-Za-z0-9_-]+:/.test(trimmed)) {
      break;
    }
    if (trimmed.indexOf('- ') === 0) {
      if (nodeIndent === null) nodeIndent = yamlIndent(line);
      if (yamlIndent(line) > nodeIndent) {
        if (current && listKey === 'alpn' && yamlIndent(line) >= listIndent) {
          if (!Array.isArray(current.alpn)) current.alpn = [];
          current.alpn.push(unquoteYamlScalar(trimmed.slice(2)));
        }
        continue;
      }
      listKey = null;
      if (current) {
        proxies.push(current);
      }
      current = {};
      rest = trimmed.slice(2).trim();
      if (rest.charAt(0) === '{') {
        current = parseYamlInlineMap(rest);
        proxies.push(current);
        current = null;
      } else if (rest) {
        assignYamlKeyValue(current, rest);
      }
      continue;
    }
    if (current && trimmed.indexOf('- ') !== 0) {
      var keyMatch = trimmed.match(/^([\w-]+):\s*$/);
      listKey = keyMatch ? keyMatch[1] : null;
      listIndent = yamlIndent(line);
      assignYamlKeyValue(current, trimmed);
    }
  }
  if (current) {
    proxies.push(current);
  }

  return proxies.map(function (proxy, proxyIndex) {
    return clashNodeFromProxy(proxy, proxyIndex);
  });
}

function canonicalConfigKey(key) {
  return String(key || '').toLowerCase().replace(/[\s_-]+/g, '');
}

function parseConfigFields(value) {
  var parts = splitYamlInlineMap(value);
  var positional = [];
  var options = {};

  parts.forEach(function (part) {
    var text = String(part || '').trim();
    var equals = text.indexOf('=');
    var key;
    var optionValue;

    if (equals > 0) {
      key = canonicalConfigKey(text.slice(0, equals));
      optionValue = text.slice(equals + 1);
      options[key] = unquoteYamlScalar(optionValue);
    } else {
      positional.push(unquoteYamlScalar(text));
    }
  });

  return { positional: positional, options: options };
}

function configOption(options, names) {
  var index;
  var key;
  for (index = 0; index < names.length; index++) {
    key = canonicalConfigKey(names[index]);
    if (options[key] !== undefined && options[key] !== '') {
      return options[key];
    }
  }
  return undefined;
}

function serverPortFromConfig(positional, options) {
  var server = configOption(options, ['server', 'address', 'host']);
  var port = configOption(options, ['port', 'server_port', 'server-port']);
  var endpoint;
  var consumed = 0;

  if (!server && positional.length > 0) {
    if (String(positional[0]).indexOf(':') >= 0) {
      endpoint = parseServerPort(String(positional[0]));
      return { server: endpoint.server, server_port: endpoint.server_port, consumed: 1 };
    }
    server = positional[0];
    consumed = 1;
  }
  if (!port && positional.length > consumed) {
    port = positional[consumed];
    consumed += 1;
  }
  if (!server) {
    throw new Error('missing server');
  }
  return { server: String(server), server_port: validatePort(port, 'server_port'), consumed: consumed };
}

function tlsQueryFromConfig(options) {
  var source = {};
  var query;
  var sni = configOption(options, ['sni', 'servername', 'server-name', 'serverName', 'tls-name', 'tlsName', 'peer']);
  var network = configOption(options, ['network', 'obfs', 'type']);
  var host = configOption(options, ['host', 'obfshost', 'obfs-host']);
  var path = configOption(options, ['path', 'obfspath', 'obfs-path']);

  Object.keys(options || {}).forEach(function (key) {
    source[key] = options[key];
  });
  query = tlsQueryFromFields(source);
  if (network) {
    query.type = network;
  }
  if (host) {
    query.host = host;
  }
  if (path) {
    query.path = path;
  }
  if (sni) {
    query.sni = sni;
  }
  return query;
}

function parseAppConfigLine(line, sourceIndex) {
  var trimmed = String(line || '').trim();
  var equals;
  var left;
  var right;
  var fields;
  var protocol;
  var name;
  var endpoint;
  var node;
  var nextIndex;
  var credential;

  if (!trimmed || trimmed.charAt(0) === '#' || trimmed.charAt(0) === ';' || trimmed.charAt(0) === '[') {
    return null;
  }
  if (/^(proxy-groups?|rules?|url-rewrite|mitm|general|dns)\s*:/i.test(trimmed)) {
    return null;
  }
  equals = trimmed.indexOf('=');
  if (equals < 0) {
    return null;
  }

  left = trimmed.slice(0, equals).trim();
  right = trimmed.slice(equals + 1).trim();
  protocol = protocolAlias(left);
  name = displayText(left);
  fields = parseConfigFields(right);

  if (protocol) {
    name = displayText(configOption(fields.options, ['tag', 'name', 'remarks']) || protocol.toUpperCase());
  } else if (fields.positional.length > 0) {
    protocol = protocolAlias(fields.positional[0]);
    fields.positional = fields.positional.slice(1);
  }

  if (!protocol) {
    return {
      status: 'unsupported',
      protocol: 'unknown',
      reason: 'unsupported app config line',
      source_index: sourceIndex
    };
  }

  try {
    name = displayText(configOption(fields.options, ['tag', 'name', 'remarks']) || name);
    endpoint = serverPortFromConfig(fields.positional, fields.options);
    nextIndex = endpoint.consumed;
    node = {
      status: 'supported',
      protocol: protocol,
      name: String(name || endpoint.server),
      country: countryForName(name || endpoint.server),
      server: endpoint.server,
      server_port: endpoint.server_port
    };

    if (protocol === 'ss') {
      node.method = configOption(fields.options, ['method', 'cipher', 'encrypt-method', 'encryptmethod']) || fields.positional[nextIndex];
      node.password = configOption(fields.options, ['password']) || fields.positional[nextIndex + 1];
      if (!node.method || !node.password) {
        throw new Error('missing shadowsocks method or password');
      }
      return node;
    }

    credential = configOption(fields.options, ['uuid', 'id', 'password', 'pass', 'auth', 'auth-str', 'auth_str', 'username']) || fields.positional[nextIndex];
    if (!credential) {
      throw new Error((protocol === 'anytls' || protocol === 'trojan' || protocol === 'hysteria2') ? 'missing password' : 'missing uuid');
    }
    if (protocol === 'anytls' || protocol === 'trojan' || protocol === 'hysteria2') {
      node.password = credential;
    } else {
      node.uuid = credential;
    }

    node.query = tlsQueryFromConfig(fields.options);
    if (protocol === 'vmess') {
      node.security = configOption(fields.options, ['cipher', 'security', 'method']) || 'auto';
      node.alter_id = Number(configOption(fields.options, ['alterId', 'alter-id', 'alter_id']) || 0);
    }
    return node;
  } catch (error) {
    return {
      status: 'unsupported',
      protocol: protocol,
      reason: String(error.message || error),
      source_index: sourceIndex
    };
  }
}

function appConfigLooksLikely(input) {
  return /\[(Proxy|server_local)\]/i.test(input) ||
    /^\s*(ss|shadowsocks|vless|vmess|anytls)\s*=/im.test(input) ||
    /=\s*(ss|shadowsocks|vless|vmess|anytls)\s*,/im.test(input);
}

function parseAppConfig(input) {
  var parsed = [];

  if (!appConfigLooksLikely(input)) {
    return null;
  }

  String(input || '').split(/\r?\n/).forEach(function (line, index) {
    var item = parseAppConfigLine(line, index);
    if (item) {
      parsed.push(item);
    }
  });
  return parsed.length ? parsed : null;
}

function subscriptionLines(input) {
  var plainLines = input.split(/\r?\n/).map(function (line) {
    return line.trim();
  }).filter(Boolean);

  if (plainLines.some(function (line) { return /^[A-Za-z][A-Za-z0-9+.-]*:\/\//.test(line); })) {
    return plainLines;
  }

  var decoded = b64decode(input);
  if (!decoded) {
    return plainLines;
  }
  return decoded.split(/\r?\n/).map(function (line) {
    return line.trim();
  }).filter(Boolean);
}

export function parseSubscription(input) {
  var clashNodes;
  var appConfigNodes;
  if (!input || !input.trim()) {
    throw new Error('empty subscription input');
  }

  var nodes = [];
  var unsupported = [];
  clashNodes = parseClashYaml(input);
  if (clashNodes) {
    clashNodes.forEach(function (parsed) {
      if (parsed.status === 'supported') {
        nodes.push(parsed);
      } else {
        unsupported.push(parsed);
      }
    });
    return { nodes: nodes, unsupported: unsupported };
    return;
  }
  appConfigNodes = parseAppConfig(input);
  if (appConfigNodes) {
    appConfigNodes.forEach(function (parsed) {
      if (parsed.status === 'supported') {
        nodes.push(parsed);
      } else {
        unsupported.push(parsed);
      }
    });
    return { nodes: nodes, unsupported: unsupported };
    return;
  }

  subscriptionLines(input).forEach(function (line, index) {
    var parsed = parseUri(line, index);
    if (parsed.status === 'supported') {
      nodes.push(parsed);
    } else {
      unsupported.push(parsed);
    }
  });

  return { nodes: nodes, unsupported: unsupported };
}
