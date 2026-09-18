import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parse, align } from '../src/subscription.ts';
import { generate } from '../src/core.ts';
import type { State, Profile } from '../src/types.ts';
const uuid='12345678-1234-1234-1234-123456789abc';
const vmess=Buffer.from(JSON.stringify({v:'2',ps:'JP vmess',add:'127.0.0.1',port:'443',id:uuid,aid:'0',net:'tcp',tls:'tls'})).toString('base64');
export const uris=[
  'anytls://test@127.0.0.1:443?sni=example.com#JP%20anytls',
  `vless://${uuid}@127.0.0.1:443?security=tls&sni=example.com#JP%20vless`,
  `vmess://${vmess}`,
  `ss://${Buffer.from('aes-128-gcm:password').toString('base64')}@127.0.0.1:8388#US%20ss`,
  'trojan://password@127.0.0.1:443?sni=example.com#US%20trojan',
  'hy2://password@127.0.0.1:443?sni=example.com#US%20hy2'
];
test('original six protocols, Base64, unsupported nodes and urltest selection survive migration',()=>{
  const plain=parse(uris.join('\n')); assert.equal(plain.nodes.length,6); assert.equal(plain.unsupported.length,0);
  assert.deepEqual(parse(Buffer.from(uris.join('\n')).toString('base64')),plain);
  assert.equal(parse(uris.join('\n')+'\nunknown://x').unsupported.length,1);
  const state:State={schemaVersion:1,apps:[],shortcuts:[],settings:{testUrl:'https://example.com',exitUrl:'https://example.com'},profiles:[{id:'p1',name:'test',kind:'managed',host:'127.0.0.1',port:19000,nodes:plain.nodes.map(n=>({...n,selected:true}))}]};
  const config=generate(state);assert.equal(config.outbounds.filter((o:any)=>o.type==='urltest')[0].outbounds.length,6);
  assert.equal(config.route.rules.at(-1).action,'reject');assert.ok(!config.outbounds.some((o:any)=>o.type==='block'));
});
test('Clash inline and nested proxy forms',()=>{
  const text=`proxies:
  - {name: US SS, type: ss, server: 127.0.0.1, port: 8080, cipher: aes-128-gcm, password: secret}
  - name: JP Trojan
    type: trojan
    server: 127.0.0.1
    port: 443
    password: secret
    sni: example.com
`;
  const r=parse(text); assert.equal(r.nodes.length,2); assert.equal(r.nodes[1].password,'secret');
});
test('Surge/Loon/Shadowrocket and Quantumult X line formats',()=>{
  for(const text of ['[Proxy]\nJP = trojan, 127.0.0.1, 443, password=secret, sni=example.com','[server_local]\ntrojan=127.0.0.1:443, password=secret, tls-host=example.com, tag=JP']){
    const r=parse(text);assert.equal(r.nodes.length,1,JSON.stringify(r));assert.equal(r.nodes[0].protocol,'trojan');
  }
});
test('Clash block ALPN lists do not become phantom nodes',()=>{
  for (const indent of ['', '  ']) {
    const text='proxies:\n'+['- name: US AnyTLS','  type: anytls','  server: example.com','  port: 443','  password: placeholder','  alpn:','  - h2','  - http/1.1','  udp: true','- name: JP Trojan','  type: trojan','  server: example.com','  port: 443','  password: placeholder','  alpn:','    - h2'].map(l=>indent+l).join('\n')+'\nproxy-groups:\n- name: ignored\n';
    const r=parse(text); assert.equal(r.nodes.length,2); assert.equal(r.unsupported.length,0);
    assert.deepEqual(r.nodes[0].query.alpn,['h2','http/1.1']); assert.deepEqual(r.nodes[1].query.alpn,['h2']);
  }
});
test('refresh preserves selection by name, reports removals, refuses empty or ambiguous selection',()=>{
  const nodes=parse(uris.join('\n')).nodes;const p:Profile={id:'p',name:'p',kind:'managed',host:'127.0.0.1',port:18099,nodes:nodes.map((n,i)=>({...n,selected:i<2}))};
  const r=align(p,nodes.slice(1));assert.equal(r.nodes.filter(n=>n.selected).length,1);assert.equal(r.removed.length,1);
  assert.throws(()=>align(p,nodes.slice(2)),/清空/);assert.throws(()=>align(p,[nodes[0],nodes[0]]),/重名/);
});
