import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import { Service } from '../src/service.ts';
import { proxyRequest, tcp } from '../src/proxy.ts';
import { childEnvironment, expectedProxy, instancePaths } from '../src/applications.ts';
import { sleep, exists } from '../src/store.ts';
import { Native } from '../src/native.ts';
import { shortcut, taskSpec } from '../src/integration.ts';
import { parse } from '../src/subscription.ts';
import type { Identity } from '../src/types.ts';
import { builtinPath } from '../src/builtin.ts';
const exec=promisify(execFile);
async function listen(server:net.Server){await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));return (server.address() as net.AddressInfo).port;}
async function freePort(){const s=net.createServer();const p=await listen(s);await new Promise<void>(r=>s.close(()=>r()));return p;}
async function until(fn:()=>Promise<boolean>,limit=10000){const end=Date.now()+limit;while(Date.now()<end){if(await fn())return;await sleep(150);}throw new Error('condition timed out');}
function upstream(name:string,events:string[]){
  const s=http.createServer((req,res)=>{
    events.push(name+' '+req.method+' '+req.url);
    const u=new URL(req.url!);
    const r=http.request(u,{method:req.method,headers:{...req.headers,host:u.host}},response=>{res.writeHead(response.statusCode!,response.headers);response.pipe(res);});
    r.on('error',()=>{res.writeHead(502);res.end();});req.pipe(r);
  });
  s.on('connect',(req,client,head)=>{
    events.push(name+' CONNECT '+req.url);const [host,port]=req.url!.split(':');
    const remote=net.connect({host,port:Number(port)},()=>{client.write('HTTP/1.1 200 Connection Established\r\n\r\n');if(head.length)remote.write(head);remote.pipe(client);client.pipe(remote);});
    remote.on('error',()=>client.destroy());client.on('error',()=>remote.destroy());client.on('close',()=>remote.destroy());
  });return s;
}
test('Windows end-to-end: real core, two exits, HTTPS, launch/guard, rollback, cleanup', {timeout:180000}, async()=>{
  await fs.mkdir('.test-data',{recursive:true});
  const root=await fs.mkdtemp(resolve('.test-data/integration-'));
  const s=await new Service(join(root,'state')).init();
  const events:string[]=[];const resources:net.Server[]=[];const childIds:Identity[]=[];
  const corePath=builtinPath;
  try{
    assert.ok(await exists(corePath),'real sing-box fixture required');
    const target=http.createServer((_req,res)=>res.end('controlled-origin'));resources.push(target);const targetPort=await listen(target);
    const url=`http://127.0.0.1:${targetPort}/controlled`;
    await s.settings({testUrl:url,exitUrl:url});await s.core.prepare();
    const a=upstream('A',events),b=upstream('B',events);resources.push(a,b);const pa=await listen(a),pb=await listen(b);
    const profileA=await s.addManaged('出口 A',await freePort(),[{name:'A',protocol:'http',server:'127.0.0.1',server_port:pa,selected:true}]);
    const profileB=await s.addManaged('出口 B',await freePort(),[{name:'B',protocol:'http',server:'127.0.0.1',server_port:pb,selected:true}]);
    await assert.rejects(()=>s.prepareProfile('missing'),/代理不存在/);
    assert.equal((await s.prepareProfile(profileA.id)).id,profileA.id);
    assert.ok(await s.core.running(),'setup starts the selected managed proxy before app registration');
    const first=await s.core.start();assert.equal((await s.core.start()).pid,first.pid);
    assert.equal((await proxyRequest(profileA,url)).body,'controlled-origin');assert.equal((await proxyRequest(profileB,url)).body,'controlled-origin');
    assert.ok(events.some(x=>x.startsWith('A ')&&x.includes(String(targetPort))),JSON.stringify(events));assert.ok(events.some(x=>x.startsWith('B ')&&x.includes(String(targetPort))),JSON.stringify(events));
    const key=join(root,'test.key'),cert=join(root,'test.crt');
    await exec('C:\\Program Files\\Git\\usr\\bin\\openssl.exe',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',key,'-out',cert,'-days','1','-subj','/CN=localhost','-addext','subjectAltName=IP:127.0.0.1'],{windowsHide:true});
    const httpsTarget=https.createServer({key:await fs.readFile(key),cert:await fs.readFile(cert)},(_req,res)=>res.end('tls-origin'));resources.push(httpsTarget);const httpsPort=await listen(httpsTarget);
    assert.equal((await proxyRequest(profileA,`https://127.0.0.1:${httpsPort}/tls`,{ca:await fs.readFile(cert,'utf8')})).body,'tls-origin');
    assert.ok(events.some(x=>x.startsWith('A CONNECT')));
    await assert.rejects(()=>proxyRequest(profileA,`https://127.0.0.1:${httpsPort}/tls`),/TLS/);
    // A normal HTTP server must not be reported healthy when used as HTTPS proxy.
    await assert.rejects(()=>proxyRequest({host:'127.0.0.1',port:targetPort},`https://127.0.0.1:${httpsPort}/tls`,{timeout:800}));
    const original=await s.store.read();
    await assert.rejects(()=>s.editProfile(profileA.id,{nodes:[{name:'bad',protocol:'not-a-protocol',server:'x',server_port:1,selected:true}]}));
    assert.deepEqual((await s.store.read()).profiles,original.profiles);
    // Valid config, dead upstream: update must restore working previous config.
    const deadUpstream=await freePort();
    await assert.rejects(()=>s.editProfile(profileA.id,{nodes:[{name:'dead',protocol:'http',server:'127.0.0.1',server_port:deadUpstream,selected:true}]}));
    assert.deepEqual((await s.store.read()).profiles,original.profiles);
    assert.equal((await proxyRequest(profileA,url)).body,'controlled-origin');
    const appDir=join(root,'中文 路径');await fs.mkdir(appDir);const exe=join(appDir,'受控 程序.exe');await fs.copyFile(process.execPath,exe);
    const childScript=fileURLToPath(new URL('./child.ts',import.meta.url));const record=join(appDir,'收到参数.json');
    const odd=['hello world','中文','quote"value','trailing\\',''];
    const app=await s.apps.add({name:'受控应用',exe,cwd:appDir,args:[childScript,record,...odd,`--request-url=${url}?from=child`],adapter:'chromium',profileId:profileA.id});
    const envBefore={...process.env};const launch=await s.apps.launch(app.id);const launched=(await s.native.identity(launch.pid))!;childIds.push(launched);
    await until(()=>exists(record));const received=JSON.parse(await fs.readFile(record,'utf8'));
    assert.deepEqual(received.args.slice(0,odd.length),odd);assert.equal(received.proxy,`http://127.0.0.1:${profileA.port}`);assert.equal(received.cwd,appDir);assert.ok(JSON.stringify({...process.env})===JSON.stringify(envBefore),'parent environment must remain unchanged');
    assert.equal(received.response.body,'controlled-origin');
    await assert.rejects(()=>s.apps.launch(app.id),/已经运行/);
    const lnk=await shortcut(s.store,s.native,app.id);assert.ok(await exists(lnk!));await shortcut(s.store,s.native,app.id,true);assert.equal(await exists(lnk!),false);
    await assert.rejects(()=>s.native.stop({...launched,created:'wrong'}),/identity changed/);assert.ok(await s.native.identity(launched.pid));
    await s.native.stop(launched);
    // Guard correction happens in a dedicated data directory, never against user applications.
    await s.store.lock(async()=>{const state=await s.store.read();state.apps[0].guard=true;await s.store.save(state);});
    const bare=spawn(exe,[childScript,record,'bare'],{cwd:appDir,windowsHide:true,stdio:'ignore'});await new Promise<void>(r=>bare.once('spawn',r));
    const bareId=(await s.native.identity(bare.pid!))!;childIds.push(bareId);await sleep(1400);await s.guard.tick();
    const guardLaunch=(await s.store.runtime()).launches[app.id];assert.notEqual(guardLaunch.identity.pid,bare.pid);childIds.push(guardLaunch.identity);
    assert.ok(expectedProxy((await s.native.identity(guardLaunch.identity.pid))!,`http://127.0.0.1:${profileA.port}`));
    await s.guard.tick();assert.ok(await s.native.identity(guardLaunch.identity.pid));
    // Run the actual background CLI, not just the tick method.
    // Interactive elevation is covered by elevated-events-live.ts. Ordinary CI must not prompt UAC.
    const nativeCall=s.native.call.bind(s.native);
    s.native.call=(async(op:string,data:any)=>op==='events-install'?{installed:true,current:true}:nativeCall(op,data)) as typeof s.native.call;
    await s.guard.enable(app.id,true);
    const daemon=await s.guard.running();assert.ok(daemon);assert.equal((await s.guard.start()).pid,daemon.pid);
    await s.native.stop(guardLaunch.identity);
    const bare2=spawn(exe,[childScript,record,'bare-daemon'],{cwd:appDir,windowsHide:true,stdio:'ignore'});await new Promise<void>(r=>bare2.once('spawn',r));
    childIds.push((await s.native.identity(bare2.pid!))!);
    await until(async()=>{const r=await s.store.runtime();return r.launches[app.id].identity.pid!==guardLaunch.identity.pid;},15000);
    const corrected2=(await s.store.runtime()).launches[app.id].identity;childIds.push(corrected2);
    await s.guard.enable(app.id,false);await until(async()=>!(await s.guard.running()));await s.native.stop(corrected2);
    // Disable guard and verify direct mode strips inherited proxies.
    await s.store.lock(async()=>{const state=await s.store.read();state.apps[0].guard=false;await s.store.save(state);});
    await s.apps.edit(app.id,{profileId:undefined});
    const direct=await s.apps.launch(app.id);const directId=(await s.native.identity(direct.pid))!;childIds.push(directId);
    await until(async()=>JSON.parse(await fs.readFile(record,'utf8')).pid===direct.pid);
    assert.equal(JSON.parse(await fs.readFile(record,'utf8')).proxy,undefined);await s.native.stop(directId);
    // Two Claude clones, a Codex clone and the ordinary process coexist on the same fixture EXE.
    const ordinary=await s.apps.launch(app.id); const ordinaryId=(await s.native.identity(ordinary.pid))!;childIds.push(ordinaryId);
    const clones=[];
    for(const [i,profile] of [profileA,profileB,profileA].entries()) {
      const capture=join(appDir,`clone-${i}.json`);
      const instance=i<2?'claude':'codex';
      const clone=await s.apps.add({name:`分身 ${i}`,exe,cwd:appDir,instance,args:[childScript,capture,`--request-url=${url}?clone=${i}`],profileId:profile.id});
      const launched=await s.apps.launch(clone.id);childIds.push((await s.native.identity(launched.pid))!);
      await until(()=>exists(capture));const received=JSON.parse(await fs.readFile(capture,'utf8'));
      const paths=instancePaths(s.store,clone);
      assert.ok(received.args.includes(`--user-data-dir=${paths.userData}`));
      if(instance==='codex') { assert.equal(received.codexHome,paths.home);assert.equal(received.userData,paths.userData);assert.equal(received.claudeHome,undefined); }
      else { assert.equal(received.claudeHome,paths.home);assert.equal(received.codexHome,undefined);assert.equal(received.userData,undefined); }
      assert.deepEqual(await fs.readdir(paths.home),[],'launcher creates empty instance home without settings or credentials');
      assert.equal(received.response.body,'controlled-origin');assert.equal(received.proxy,`http://127.0.0.1:${profile.port}`);
      await assert.rejects(()=>s.apps.launch(clone.id),/已经运行/);clones.push({clone,launched,capture});
    }
    const chosen=clones[0];await s.native.stop((await s.native.identity(chosen.launched.pid))!);
    await s.store.lock(async()=>{const state=await s.store.read();state.apps.find(a=>a.id===chosen.clone.id)!.guard=true;await s.store.save(state);});
    const wrong=spawn(exe,[childScript,chosen.capture,`--user-data-dir=${instancePaths(s.store,chosen.clone).userData}`],{windowsHide:true,stdio:'ignore'});await new Promise<void>(r=>wrong.once('spawn',r));childIds.push((await s.native.identity(wrong.pid!))!);
    await sleep(1400);await s.guard.tick();const fixed=(await s.store.runtime()).launches[chosen.clone.id].identity;childIds.push(fixed);
    assert.notEqual(fixed.pid,wrong.pid);assert.ok(expectedProxy(fixed,`http://127.0.0.1:${profileA.port}`));
    assert.ok(await s.native.identity(ordinary.pid),'ordinary instance survives clone Guard');
    assert.ok(await s.native.identity(clones[1].launched.pid),'other clone survives Guard');
    assert.ok(await s.native.identity(clones[2].launched.pid),'Codex clone survives Claude Guard');
    await s.guard.enable(chosen.clone.id,false);await s.native.stop(fixed);await s.native.stop(ordinaryId);
    for(const other of clones.slice(1)) await s.native.stop((await s.native.identity(other.launched.pid))!);
    // An unavailable managed upstream must fail startup before a target is created.
    await s.core.stop();
    const dead=await s.addManaged('dead',await freePort(),[{name:'dead',protocol:'http',server:'127.0.0.1',server_port:await freePort(),selected:true}]);
    await s.apps.edit(app.id,{profileId:dead.id});
    await assert.rejects(()=>s.apps.launch(app.id),/自动网卡选择及代理联网验证失败/);assert.equal((await s.native.processes([exe])).length,0);
    assert.equal(await s.core.running(),undefined);
    // The same saved dead profile must not block a healthy app or general core startup.
    await s.apps.edit(app.id,{profileId:profileA.id});
    const healthyLaunch=await s.apps.launch(app.id);const healthyIdentity=(await s.native.identity(healthyLaunch.pid))!;childIds.push(healthyIdentity);
    assert.equal((await proxyRequest(profileA,url)).body,'controlled-origin');
    await s.native.stop(healthyIdentity);
    await s.core.stop();await s.core.start();
    assert.equal((await proxyRequest(profileB,url)).body,'controlled-origin');
    await s.apps.edit(app.id,{profileId:dead.id});
    await assert.rejects(()=>s.apps.launch(app.id),/代理/);
    assert.ok(await s.core.running(),'bad target must not stop the running core used by healthy apps');
    await assert.rejects(()=>s.removeProfile(dead.id),/引用/);
    // Scheduled task registration/unregistration: create only our unique test task.
    const spec=taskSpec(s.store);await s.native.call('task-install',spec);await s.native.call('task-install',spec);await s.native.call('task-remove',spec);
    await s.uninstall('all');await s.uninstall('all');assert.ok(await exists(exe));assert.ok(await exists(corePath));assert.ok(await tcp('127.0.0.1',pa));
    assert.ok(await exists(s.store.path('config/backup.json')));assert.ok(!(await s.core.running()));
    console.log('EVIDENCE',JSON.stringify({core:'1.14.1',httpTwoExits:true,httpsConnect:true,argumentRoundTrip:true,guardCorrection:true,rollback:true,shortcuts:true,tasks:true}));
  } finally {
    for(const id of childIds)await s.native.stop(id).catch(()=>{});
    await s.core.stop().catch(()=>{});await s.native.call('task-remove',taskSpec(s.store)).catch(()=>{});s.close();
    for(const server of resources){if('closeAllConnections' in server)(server as http.Server).closeAllConnections();server.close();}
  }
});
test('proxy environment is case-insensitive and helper/conflicting args are not accepted',()=>{
  const env=childEnvironment('http://127.0.0.1:18099');assert.equal(env.NO_PROXY,'');assert.equal(Object.keys(env).filter(k=>k.toUpperCase()==='HTTP_PROXY').length,1);
  assert.ok(!Object.keys(childEnvironment()).some(k=>['HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY'].includes(k.toUpperCase())));
  const id={args:['app.exe','--proxy-server=x','--no-proxy-server']} as Identity;assert.equal(expectedProxy(id,'x'),false);
});
