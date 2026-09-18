// Reproducible migration: preserve the original parser and generator algorithms.
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
const source = new URL('../../AppProxyInstaller-scripts/Resources/', import.meta.url);
const dest = new URL('../src/legacy/', import.meta.url);
let parser = readFileSync(new URL('app-proxy-subscription-parser.jxa', source), 'utf8');
parser = parser.slice(parser.indexOf('function safeDecode'));
parser = parser.slice(0, parser.indexOf('\ntry {\n  main();'));
parser = parser.replace('function main() {\n  var input = readStdin();', 'export function parseSubscription(input) {');
parser = parser.replaceAll('fail(\'empty subscription input\');', "throw new Error('empty subscription input');");
parser = parser.replaceAll('writeStdout(JSON.stringify({ nodes: nodes, unsupported: unsupported }, null, 2));', 'return { nodes: nodes, unsupported: unsupported };');
// Real Clash subscriptions use block ALPN lists. Only proxy-level dashes begin nodes.
parser = parser.replace('  var proxies = [];', '  var proxies = [];\n  var nodeIndent = null;\n  var listKey = null;\n  var listIndent = 0;');
parser = parser.replace("    if (trimmed.indexOf('- ') === 0) {", `    if (trimmed.indexOf('- ') === 0) {
      if (nodeIndent === null) nodeIndent = yamlIndent(line);
      if (yamlIndent(line) > nodeIndent) {
        if (current && listKey === 'alpn' && yamlIndent(line) >= listIndent) {
          if (!Array.isArray(current.alpn)) current.alpn = [];
          current.alpn.push(unquoteYamlScalar(trimmed.slice(2)));
        }
        continue;
      }
      listKey = null;`);
parser = parser.replace('      assignYamlKeyValue(current, trimmed);', `      var keyMatch = trimmed.match(/^([\\w-]+):\\s*$/);
      listKey = keyMatch ? keyMatch[1] : null;
      listIndent = yamlIndent(line);
      assignYamlKeyValue(current, trimmed);`);
const header = '// Migrated from supplied macOS JXA. See scripts/migrate-legacy.ts.\n';
parser = header + `function b64decode(value) {
  const normalized = String(value || '').replace(/\\s+/g, '').replace(/-/g, '+').replace(/_/g, '/');
  if (!normalized || !/^[A-Za-z0-9+/]*={0,2}$/.test(normalized)) return null;
  try { return new TextDecoder('utf-8', { fatal: true }).decode(Buffer.from(normalized, 'base64')); }
  catch { return null; }
}\n` + parser;
writeFileSync(new URL('subscription.js', dest), parser);
let generator = readFileSync(new URL('app-proxy-config-generator.jxa', source), 'utf8');
generator = generator.slice(generator.indexOf('function countryCode'));
generator = generator.slice(0, generator.indexOf('\nfunction main()'));
generator = generator.replace('function buildManifestConfig(payload)', 'export function buildManifestConfig(payload)');
// Block outbound was removed in newer sing-box; use a final reject rule instead.
generator = generator.replace("outbounds: outbounds.concat([{ type: 'direct', tag: 'direct' }, { type: 'block', tag: 'block' }]),", "outbounds: outbounds.concat([{ type: 'direct', tag: 'direct' }]),");
generator = generator.replace('final: finalTag,', "final: 'direct',");
// Keep the original auto-detection fallback; Windows applies its detected alias in core.ts.
generator = generator.replace('route.rules = rules;', "route.rules = rules.map(function(rule) { return Object.assign({action: 'route'}, rule); }).concat([{action: 'reject'}]);");
writeFileSync(new URL('config.js', dest), header + generator);
console.log('Migrated subscription.js and config.js');
