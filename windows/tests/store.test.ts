import { test } from 'node:test';
import assert from 'node:assert/strict';
import { validate } from '../src/store.ts';
import type { State } from '../src/types.ts';

test('profiles require managed type, loopback inbound and selected nodes; unsupported data is rejected', () => {
  const state: State = { schemaVersion: 1, apps: [], shortcuts: [], settings: { testUrl: 'https://example.com', exitUrl: 'https://example.com' }, profiles: [{ id: 'fixture', name: 'fixture', kind: 'managed', host: '127.0.0.1', port: 19000, nodes: [{ name: 'node', protocol: 'http', server: '127.0.0.1', server_port: 19001, selected: true }] }] };
  validate(state);
  for (const kind of ['external', 'unknown', undefined]) {
    const invalid = structuredClone(state); (invalid.profiles[0] as any).kind = kind;
    assert.throws(() => validate(invalid), /代理类型无效/);
  }
  const remote = structuredClone(state); remote.profiles[0].host = '192.0.2.1';
  assert.throws(() => validate(remote), /回环地址/);
  const noNodes = structuredClone(state); delete (noNodes.profiles[0] as any).nodes;
  assert.throws(() => validate(noNodes), /至少选择一个/);
});
