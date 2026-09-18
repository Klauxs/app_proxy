import {test} from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {resolve,join,dirname} from 'node:path';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {Store,exists,sleep,atomic} from '../src/store.ts';
import {Native} from '../src/native.ts';
import {launchPackage} from '../src/msix.ts';
import type {App,Identity} from '../src/types.ts';
import {Service} from '../src/service.ts';
import {applicationRoot} from '../src/msix-storage.ts';
import {instancePaths} from '../src/applications.ts';

test('MSIX discovery is independent of Codex and refreshes registered package paths', async () => {
  const service = new Service(resolve('.test-data/msix-resolution'));
  const originalExe = 'C:\\Program Files\\WindowsApps\\Example.Editor_1.0_x64__fixture\\app\\Editor.exe';
  const upgradedExe = 'D:\\WindowsApps\\Example.Editor_2.0_x64__fixture\\app\\Editor.exe';
  const app: App = { id:'editor', name:'Example Editor', exe:originalExe, cwd:dirname(originalExe), args:[], adapter:'environment', guard:false };
  let requests: any[] = []; let registration: any = { familyName:'Example.Editor_fixture', appId:'EditorApp', exe:originalExe, aumid:'Example.Editor_fixture!EditorApp', fullTrust:true };
  service.native.call = (async (op:string, data:any) => { assert.equal(op,'package-resolve'); requests.push(data); return registration; }) as Native['call'];
  assert.equal(await service.apps.resolvePackage(app),true);
  assert.deepEqual(requests[0],{exe:originalExe});
  assert.deepEqual(app.package,{familyName:'Example.Editor_fixture',appId:'EditorApp'});
  assert.equal(app.instance,undefined);
  registration = {...registration,exe:upgradedExe};
  assert.equal(await service.apps.resolvePackage(app),true);
  assert.deepEqual(requests[1],app.package); assert.equal(app.exe,upgradedExe); assert.equal(app.cwd,dirname(upgradedExe));
  app.cwd = 'D:\\Custom Working Directory'; registration = {...registration,exe:originalExe};
  await service.apps.resolvePackage(app); assert.equal(app.cwd,'D:\\Custom Working Directory');
  registration = {...registration,fullTrust:false};
  await assert.rejects(()=>service.apps.resolvePackage(app),/full-trust/);
  registration = null; await assert.rejects(()=>service.apps.resolvePackage(app),/未找到/);
  const count = requests.length;
  assert.equal(await service.apps.resolvePackage({...app,package:undefined,exe:'D:\\Tools\\Editor.exe'}),false);
  assert.equal(requests.length,count,'ordinary EXEs do not enter package discovery');
  registration = { familyName:'Example.Editor_fixture',appId:'EditorApp',exe:originalExe,fullTrust:true,isolatedStorage:true };
  await service.apps.resolvePackage(app);
  const allocated = applicationRoot(service.store,app);
  assert.match(allocated,/LocalState[\\/]AppProxy/);
  assert.ok(instancePaths(service.store,app).userData.startsWith(allocated));
  assert.notEqual(applicationRoot(new Store(resolve('.test-data/another-home')),app),allocated);
  registration.isolatedStorage = false;
  await service.apps.resolvePackage(app);
  assert.equal(applicationRoot(service.store,app),allocated,'package updates retain existing clone data location');
});

for (const kind of ['codex','claude'] as const) for (const isolatedStorage of [false,true]) test(`MSIX ${kind} isolatedStorage=${isolatedStorage} child handoff carries scoped environment, rejects stale requests and prevents duplicate retries`,{timeout:30000},async()=>{
  await fs.mkdir('.test-data',{recursive:true});const root=await fs.mkdtemp(resolve('.test-data/msix-'));
  const native=new Native();const store=new Store(join(root,'data'));await store.init(native);
  let target:Identity|undefined;
  try {
    const record=join(root,'received.json');const userData=store.path('instances','test','user-data');const appHome=store.path('instances','test',kind+'-home');
    const app:App={id:'test',name:'fixture',exe:process.execPath,cwd:root,args:[],adapter:'chromium',guard:false,instance:kind,package:{familyName:'Fixture_test',appId:'App',isolatedStorage}};
    let expire=false;
    const transport={call:async(op:string,data:any)=>{
      if(op==='protect') return native.call(op,data);
      assert.equal(op,'package-launch');assert.equal(data.familyName,'Fixture_test');
      const pending=JSON.parse(await fs.readFile(store.path('state/msix-pending-test.json'),'utf8'));
      assert.equal(dirname(pending.requestPath),join(applicationRoot(store,app),'state'));
      if(expire){const req=JSON.parse(await fs.readFile(pending.requestPath,'utf8'));req.expiresAt=Date.now()-1;await atomic(pending.requestPath,req);}
      await promisify(execFile)(process.execPath,[resolve('src/msix-child.ts'),pending.requestPath],{windowsHide:true,timeout:10000});
      return true;
    }} as Native;
    const args=[resolve('tests/child.ts'),record,'中文 value','quote"value',`--user-data-dir=${userData}`];
    const env:NodeJS.ProcessEnv={HTTP_PROXY:'http://127.0.0.1:18099',HTTPS_PROXY:'http://127.0.0.1:18099',...(kind==='codex'?{CODEX_HOME:appHome,CODEX_ELECTRON_USER_DATA_PATH:userData}:{CLAUDE_CONFIG_DIR:appHome})};
    const pid=await launchPackage(store,transport,app,args,env);target=await native.identity(pid);assert.ok(target);
    const deadline=Date.now()+5000;while(!await exists(record)&&Date.now()<deadline)await sleep(100);
    const received=JSON.parse(await fs.readFile(record,'utf8'));assert.equal(received.pid,pid);assert.equal(received.proxy,env.HTTP_PROXY);assert.equal(received.cwd,root);assert.deepEqual(received.args.slice(0,2),args.slice(2,4));
    if(kind==='codex') {assert.equal(received.codexHome,appHome);assert.equal(received.userData,userData);assert.ok(!received.claudeHome);}
    else {assert.equal(received.claudeHome,appHome);assert.ok(!received.codexHome);assert.ok(!received.userData);}
    assert.equal(await exists(store.path('state/msix-pending-test.json')),false);
    await native.stop(target);target=undefined;
    expire=true;await assert.rejects(()=>launchPackage(store,transport,app,args,env),/LAUNCH_EXPIRED/);
    await atomic(store.path('state/msix-pending-test.json'),{expiresAt:Date.now()+10000});
    await assert.rejects(()=>launchPackage(store,transport,app,args,env),/仍待确认/);
  }finally{if(target)await native.stop(target);native.close();}
});
