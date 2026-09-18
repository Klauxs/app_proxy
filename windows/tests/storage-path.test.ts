import {test} from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {join,resolve,basename} from 'node:path';
import {Store,atomic} from '../src/store.ts';
import {Native} from '../src/native.ts';
import {launchSpec,taskSpec} from '../src/integration.ts';
import {applicationRoot} from '../src/msix-storage.ts';
import {appProcesses,instancePaths} from '../src/applications.ts';
import type {App,Identity} from '../src/types.ts';

test('redirected storage is shared with desktop launchers while instance scope survives reopening',async()=>{
  const parent=await fs.mkdtemp(resolve('.test-data/storage-path-'));
  const logical=join(parent,'logical'),physical=join(parent,'physical');
  await fs.mkdir(physical);
  await fs.writeFile(join(physical,'.app-proxy-owned'),'Windows App Proxy v1\n');
  const app:App={id:'clone',name:'clone',exe:process.execPath,cwd:parent,args:[],adapter:'chromium',instance:'codex',guard:false};
  await atomic(join(physical,'manifest.json'),{schemaVersion:1,apps:[app],profiles:[],settings:{testUrl:'https://example.com',exitUrl:'https://example.com'},shortcuts:[]});
  const native=new Native();const call=native.call.bind(native);
  native.call=((op:string,data:any)=>op==='physical-file'?Promise.resolve(join(physical,basename(data.path))):call(op,data)) as Native['call'];
  const old=new Store(logical);const store=new Store(logical);
  const packaged={...app,package:{familyName:'Example_fixture',appId:'App',isolatedStorage:true}};
  try{
    await store.init(native);
    assert.equal(store.root,physical);assert.equal(store.scopeRoot,logical);
    assert.equal((await store.read()).apps[0].id,app.id);
    assert.ok(launchSpec(store,['launch',app.id]).arguments.includes(`"--home" "${physical}"`));
    assert.equal(taskSpec(store).name,taskSpec(old).name);
    assert.equal(applicationRoot(store,packaged),applicationRoot(old,packaged));
    const id:Identity={pid:1,parent:0,path:app.exe,args:[`--user-data-dir=${instancePaths(old,app).userData}`],created:new Date().toISOString(),session:1,owned:true};
    assert.equal(appProcesses(store,app,[id]).length,1,'recognize the clone still carrying its old logical path');
    assert.equal(appProcesses(store,{...app,id:'another'},[id]).length,0);
    native.call=call;
    const reopened=new Store(physical);await reopened.init(native);
    assert.equal(reopened.scopeRoot,logical);assert.equal(reopened.root,physical);
    assert.deepEqual(await reopened.read(),await store.read());
    assert.equal(applicationRoot(reopened,packaged),applicationRoot(old,packaged));
  }finally{native.close()}
});

test('default entry points reuse the shared locator while explicit homes remain independent',async()=>{
  const parent=await fs.mkdtemp(resolve('.test-data/default-home-'));
  const original={USERPROFILE:process.env.USERPROFILE,LOCALAPPDATA:process.env.LOCALAPPDATA,APP_PROXY_HOME:process.env.APP_PROXY_HOME};
  const native=new Native();
  try {
    process.env.USERPROFILE=parent;process.env.LOCALAPPDATA=join(parent,'package-view');delete process.env.APP_PROXY_HOME;
    const first=new Store();await first.init(native);
    const state=await first.read();state.apps.push({id:'registered',name:'registered',exe:process.execPath,cwd:parent,args:[],adapter:'environment',guard:false});await first.save(state);
    process.env.LOCALAPPDATA=join(parent,'ordinary-view');
    const second=new Store();await second.init(native);
    assert.equal(second.root,first.root);assert.equal((await second.read()).apps[0].id,'registered');
    const explicit=new Store(join(parent,'explicit'));await explicit.init(native);
    assert.notEqual(explicit.root,first.root);assert.equal((await explicit.read()).apps.length,0);
    const third=new Store();await third.init(native);assert.equal(third.root,first.root);
  } finally {
    for(const [key,value] of Object.entries(original)) {if(value===undefined)delete process.env[key];else process.env[key]=value;}
    native.close();
  }
});
