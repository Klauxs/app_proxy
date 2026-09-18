import { test } from 'node:test';
import assert from 'node:assert/strict';
import { physicalCandidates, upstreamChoices, type NetworkAdapter } from '../src/network.ts';
import { generate } from '../src/core.ts';
import type { Native } from '../src/native.ts';
import type { State } from '../src/types.ts';
import * as fs from 'node:fs/promises';
import { resolve } from 'node:path';
import http from 'node:http';
import net from 'node:net';
import { Service } from '../src/service.ts';
import { builtinPath } from '../src/builtin.ts';
import { proxyRequest } from '../src/proxy.ts';

test('automatic interface selection excludes tunnels and disconnected/link-local NICs, prefers Wi-Fi then route metric', () => {
  const base: NetworkAdapter = { name: '以太网', index: 10, up: true, hardware: true, virtual: false, wifi: false, addresses: ['192.168.1.5'], metric: 20 };
  const adapters: NetworkAdapter[] = [base,
    { ...base, name: 'Wi-Fi 2', index: 2, wifi: true, metric: 50 },
    { ...base, name: 'Wi-Fi', index: 3, wifi: true, metric: 25 },
    { ...base, name: 'Ethernet fast', index: 4, metric: 10 },
    { ...base, name: 'TUN', index: 5, metric: 0, hardware: false, virtual: true },
    { ...base, name: 'Corp Wintun', index: 6, metric: 0 },
    { ...base, name: 'unknown virtual', index: 7, virtual: true },
    { ...base, name: 'Disconnected', up: false },
    { ...base, name: 'No gateway', metric: null },
    { ...base, name: 'Link-local', addresses: ['169.254.1.2'] }
  ];
  assert.deepEqual(physicalCandidates(adapters).map(a => a.name), ['Wi-Fi', 'Wi-Fi 2', 'Ethernet fast', '以太网']);
});

test('empty/failed enumeration keeps explicit, visible auto-route fallback', async () => {
  for (const call of [async () => [], async () => { throw new Error('CIM unavailable'); }]) {
    const choices = await upstreamChoices({ call } as unknown as Native);
    assert.equal(choices.length, 1); assert.equal(choices[0].mode, 'auto'); assert.match(choices[0].reason!, /可能经过虚拟网卡/);
  }
});

test('Unicode physical interface and auto fallback are mutually exclusive in generated core config', () => {
  const state: State = { schemaVersion: 1, profiles: [{ id: 'fixture', name: 'fixture', kind: 'managed', host: '127.0.0.1', port: 19001, nodes: [{ name: 'node', protocol: 'http', server: '127.0.0.1', server_port: 19002, selected: true }] }], apps: [], shortcuts: [], settings: { testUrl: 'https://example.com', exitUrl: 'https://example.com' } };
  const bound = generate(state, { mode: 'physical', interface: '以太网 2' });
  assert.equal(bound.route.default_interface, '以太网 2'); assert.equal(bound.route.auto_detect_interface, undefined);
  const fallback = generate(state); assert.equal(fallback.route.auto_detect_interface, true); assert.equal(fallback.route.default_interface, undefined);
});

test('real bundled core retries failed NIC, exposes fallback, re-detects on restart and ignores legacy executable', { timeout: 60000 }, async () => {
  await fs.mkdir('.test-data', { recursive: true });
  const root = await fs.mkdtemp(resolve('.test-data/network-'));
  const service = await new Service(root).init();
  const upstream = http.createServer((_req, res) => res.end('through-bundled-core'));
  await new Promise<void>(r => upstream.listen(0, '0.0.0.0', r));
  const upstreamPort = (upstream.address() as net.AddressInfo).port;
  upstream.on('connect', (_req, client, head) => {
    const remote = net.connect(upstreamPort, '127.0.0.1', () => { client.write('HTTP/1.1 200 Connection Established\r\n\r\n'); if (head.length) remote.write(head); remote.pipe(client); client.pipe(remote); });
    client.on('error', () => remote.destroy()); client.on('close', () => remote.destroy()); remote.on('error', () => client.destroy());
  });
  const url = `http://127.0.0.1:${upstreamPort}/health`;
  const temp = net.createServer(); await new Promise<void>(r => temp.listen(0, '127.0.0.1', r));
  const listenPort = (temp.address() as net.AddressInfo).port; await new Promise<void>(r => temp.close(() => r()));
  const originalCall = service.native.call.bind(service.native); let enumerations = 0;
  const localAddress = physicalCandidates(await originalCall<NetworkAdapter[]>('network-adapters'))[0]?.addresses[0];
  service.native.call = (async (op: string, data?: any) => {
    if (op !== 'network-adapters') return originalCall(op, data);
    enumerations++;
    return [{ name: 'AppProxy Missing NIC', index: 99999, up: true, hardware: true, virtual: false, wifi: true, addresses: ['192.0.2.1'], metric: 1 }];
  }) as Native['call'];
  try {
    assert.ok(localAddress, 'fixture requires a connected physical NIC for non-loopback interface binding');
    await service.settings({ testUrl: url });
    const profile = await service.addManaged('fixture', listenPort, [{ name: 'fixture', protocol: 'http', server: localAddress, server_port: upstreamPort, selected: true }]);
    await service.store.lock(async () => { const state = await service.store.read(); state.core = { path: process.execPath, version: 'legacy arbitrary executable' }; await service.store.save(state); });
    const id = await service.core.start(); assert.equal(id.path.toLowerCase(), builtinPath.toLowerCase());
    assert.equal((await proxyRequest(profile, url)).body, 'through-bundled-core');
    assert.equal((await service.store.runtime()).upstream?.mode, 'auto');
    assert.match((await service.store.runtime()).upstream?.reason || '', /可能经过虚拟网卡/);
    assert.match(await fs.readFile(service.store.path('logs/events.log'), 'utf8'), /upstream-failed AppProxy Missing NIC/);
    const before = enumerations; const next = await service.core.restart();
    assert.notEqual(next.pid, id.pid); assert.ok(enumerations > before);
    assert.equal((await service.store.read()).core?.bundled, true);
  } finally { await service.core.stop(); service.close(); upstream.closeAllConnections(); await new Promise<void>(r => upstream.close(() => r())); }
});
