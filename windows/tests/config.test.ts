import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { generate } from '../src/core.ts';
import { parse } from '../src/subscription.ts';
import { builtinPath } from '../src/builtin.ts';
const exec=promisify(execFile);
test('six migrated protocol outbounds accepted by actual sing-box',async()=>{
  const uri=`anytls://test@127.0.0.1:443?sni=example.com#JP-anytls
vless://12345678-1234-1234-1234-123456789abc@127.0.0.1:443?security=tls#JP-vless
vmess://${Buffer.from(JSON.stringify({v:'2',ps:'JP-vmess',add:'127.0.0.1',port:'443',id:'12345678-1234-1234-1234-123456789abc',aid:0,net:'tcp',tls:'tls'})).toString('base64')}
ss://${Buffer.from('aes-128-gcm:secret').toString('base64')}@127.0.0.1:8388#US-ss
trojan://secret@127.0.0.1:443?sni=example.com#US-trojan
hysteria2://secret@127.0.0.1:443?sni=example.com#US-hy2`;
  const nodes=parse(uri).nodes;assert.equal(nodes.length,6);
  const config=generate({schemaVersion:1,apps:[],shortcuts:[],settings:{testUrl:'https://example.com',exitUrl:'https://example.com'},profiles:[{id:'six',name:'six',kind:'managed',host:'127.0.0.1',port:19000,nodes:nodes.map(n=>({...n,selected:true}))}]});
  await mkdir('.test-data',{recursive:true});const dir=await mkdtemp(resolve('.test-data/config-'));const path=join(dir,'config.json');await writeFile(path,JSON.stringify(config));
  await exec(builtinPath,['check','-c',path],{windowsHide:true,timeout:10000});
});
