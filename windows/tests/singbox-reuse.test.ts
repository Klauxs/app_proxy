import {test} from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {spawn,execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {pathToFileURL} from 'node:url';
import http from 'node:http';
import net from 'node:net';
import {Service} from '../src/service.ts';
import {builtinDirectory,builtinPath} from '../src/builtin.ts';
import {Native} from '../src/native.ts';
import {sleep,exists} from '../src/store.ts';
import {tcp,proxyRequest} from '../src/proxy.ts';
import type {Discovery} from '../src/singbox.ts';
import type {Identity} from '../src/types.ts';
const exec=promisify(execFile);
async function freePort(){const server=net.createServer();await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));const port=(server.address() as net.AddressInfo).port;await new Promise<void>(r=>server.close(()=>r()));return port;}

test('reuse an independent sing-box service and executable without adopting its config or process',{timeout:60000},async()=>{
  const root=await fs.mkdtemp(resolve('.test-data/singbox-reuse-'));
  const program=join(root,'installed');await fs.cp(builtinDirectory,program,{recursive:true});
  const binary=join(program,'sing-box.exe');const port=await freePort();
  const config=join(root,'existing-config.json');
  const contents=JSON.stringify({log:{disabled:true},inbounds:[{type:'http',tag:'existing',listen:'127.0.0.1',listen_port:port}],outbounds:[{type:'direct',tag:'direct'}],route:{final:'direct'}});
  await fs.writeFile(config,contents);
  const origin=http.createServer((_req,res)=>res.end('127.0.0.9'));
  await new Promise<void>(r=>origin.listen(0,'127.0.0.1',r));const originPort=(origin.address() as net.AddressInfo).port;
  origin.on('connect',(_req,client,head)=>{
    const remote=net.connect(originPort,'127.0.0.1',()=>{client.write('HTTP/1.1 200 Connection Established\r\n\r\n');if(head.length)remote.write(head);remote.pipe(client);client.pipe(remote);});
    client.on('error',()=>remote.destroy());client.on('close',()=>remote.destroy());remote.on('error',()=>client.destroy());
  });
  const url=`http://127.0.0.1:${originPort}/health`;
  const independent=spawn(binary,['run','-c',config],{cwd:program,windowsHide:true,stdio:'ignore'});
  await new Promise<void>((r,j)=>{independent.once('spawn',r);independent.once('error',j);});
  const s=await new Service(join(root,'tool')).init();let child:Identity|undefined;
  const call=s.native.call.bind(s.native);
  s.native.call=(async(op:string,data:any)=>{
    const result=await call(op,data);
    if(op!=='singbox-discover')return result;
    const found=result as Discovery;
    // Keep this integration test from probing unrelated user proxies.
    return {binaries:found.binaries.filter(p=>p.toLowerCase()===binary.toLowerCase()),listeners:found.listeners.filter(p=>p.pid===independent.pid)};
  }) as Native['call'];
  try {
    const deadline=Date.now()+5000;while(!(await tcp('127.0.0.1',port))&&Date.now()<deadline)await sleep(100);
    await s.settings({testUrl:url,exitUrl:url});
    const found=await s.discoverSingBox();assert.equal(found.available[0].port,port);
    const reused=await s.useSingBox('existing',port);assert.equal(reused.kind,'sing-box');
    assert.equal(await exists(s.store.path('config/sing-box.json')),false);
    assert.equal(await s.core.running(),undefined);
    await assert.rejects(()=>s.useSingBox('ordinary-http',originPort),/不是当前可识别/);
    await assert.rejects(()=>s.editProfile(reused.id,{port:port+1}),/原服务管理/);
    await assert.rejects(()=>s.refresh(reused.id),/不是本工具管理/);
    const selected=await s.core.prepare();assert.equal(selected.path,binary);assert.equal(selected.bundled,undefined);
    const managed=await s.addManaged('own',await freePort(),[{name:'local',protocol:'http',server:'127.0.0.1',server_port:originPort,selected:true}]);
    const own=await s.core.start();assert.equal(own.path,binary);assert.notEqual(own.pid,independent.pid);
    const generated=JSON.parse(await fs.readFile(s.store.path('config/sing-box.json'),'utf8'));
    assert.deepEqual(generated.inbounds.map((p:any)=>p.listen_port),[managed.port]);
    await s.editProfile(reused.id,{name:'renamed'});assert.equal((await s.core.running())?.pid,own.pid);
    const exe=join(root,'fixture.exe');await fs.copyFile(process.execPath,exe);
    const capture=join(root,'child.json');
    const app=await s.apps.add({name:'reused',exe,adapter:'chromium',profileId:reused.id,args:[resolve('tests/child.ts'),capture,`--request-url=${url}`]});
    const launched=await s.apps.launch(app.id);child=await s.native.identity(launched.pid);
    const end=Date.now()+5000;while(!(await exists(capture))&&Date.now()<end)await sleep(100);
    assert.equal(JSON.parse(await fs.readFile(capture,'utf8')).response.body,'127.0.0.9');
    await s.store.lock(async()=>{const state=await s.store.read();state.apps[0].guard=true;await s.store.save(state);});
    await s.uninstall('core');assert.equal((await s.store.read()).apps[0].guard,true);
    assert.equal(await s.core.running(),undefined);assert.ok(await s.native.identity(independent.pid!));
    assert.equal((await proxyRequest(reused,url)).body,'127.0.0.9');
    await s.uninstall('all');assert.ok(await s.native.identity(independent.pid!));
    assert.equal(await fs.readFile(config,'utf8'),contents);assert.ok(await exists(binary));
  } finally {if(child)await s.native.stop(child);await s.core.stop();s.close();independent.kill();origin.closeAllConnections();await new Promise<void>(r=>origin.close(()=>r()));}
});

test('missing executable installs the verified official standalone core from download cache',{timeout:30000},async()=>{
  const root=await fs.mkdtemp(resolve('.test-data/singbox-install-'));
  const tool=join(root,'portable');await fs.mkdir(tool);
  for(const dir of ['src','native'])await fs.cp(resolve(dir),join(tool,dir),{recursive:true});
  await fs.copyFile(resolve('package.json'),join(tool,'package.json'));
  const home=join(root,'data'),cache=join(home,'bin','download');
  const initial=await new Service(home).init();initial.close();await fs.mkdir(cache,{recursive:true});
  await fs.copyFile(resolve('.tools/sing-box.zip'),join(cache,'sing-box.zip'));
  const moduleUrl=pathToFileURL(join(tool,'src/service.ts')).href;
  const code=`import {Service} from ${JSON.stringify(moduleUrl)};const s=await new Service(${JSON.stringify(home)}).init();const call=s.native.call.bind(s.native);s.native.call=(op,data)=>op==='singbox-discover'?Promise.resolve({binaries:[],listeners:[]}):call(op,data);try{console.log(JSON.stringify(await s.core.prepare()));}finally{s.close();}`;
  const {stdout}=await exec(process.execPath,['--input-type=module','-e',code],{windowsHide:true,timeout:20000});
  const info=JSON.parse(stdout);assert.equal(info.downloaded,true);assert.ok(info.path.startsWith(home));assert.notEqual(info.path,builtinPath);
  assert.match((await exec(info.path,['version'],{windowsHide:true})).stdout,/sing-box version/);
  assert.ok(await exists(join(info.path,'..','libcronet.dll')));
});
