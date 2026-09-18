import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { Store } from '../src/store.ts';
import { Applications, appProcesses, instancePaths } from '../src/applications.ts';
import type { App, Identity } from '../src/types.ts';

for (const kind of ['codex','claude'] as const) test(`${kind} instance matching keeps ordinary and other-profile helpers outside clone Guard`,()=>{
  const store=new Store(resolve('.test-data/scope'));
  const base:App={id:'ordinary',name:'ordinary',exe:resolve('app.exe'),cwd:resolve('.'),args:[],adapter:'chromium',guard:false};
  const a:App={...base,id:'clone-a',instance:kind};const b:App={...a,id:'clone-b'};
  const process=(pid:number,parent:number,args:string[]):Identity=>({pid,parent,args,path:base.exe,created:new Date().toISOString(),session:1,owned:true});
  const live=[process(1,0,[]),process(2,1,['--type=renderer']),process(3,0,[`--user-data-dir=${instancePaths(store,a).userData}`]),process(4,3,['--type=gpu-process']),process(5,4,['--type=renderer']),process(6,0,['--user-data-dir',instancePaths(store,b).userData]),process(7,6,['--type=renderer'])];
  assert.deepEqual(appProcesses(store,base,live).map(p=>p.pid),[1,2]);
  assert.deepEqual(appProcesses(store,a,live).map(p=>p.pid),[3,4,5]);
  assert.deepEqual(appProcesses(store,b,live).map(p=>p.pid),[6,7]);
  assert.equal(appProcesses(store,a,[process(9,0,[`--user-data-dir=${instancePaths(store,a).userData}`,'--user-data-dir=elsewhere'])]).length,0);
});

test('both clone adapters reject overriding their profile or proxy arguments',()=>{
  const check = Applications.prototype.checkArgs;
  for (const instance of ['codex','claude'] as const) {
    const app: App = {id:'fixture',name:'fixture',exe:resolve('app.exe'),cwd:resolve('.'),adapter:'chromium',guard:false,instance,args:[]};
    check(app);
    for (const args of [['--user-data-dir=other'],['--user-data-dir','other'],['--proxy-server=http://other'],['--']]) assert.throws(()=>check({...app,args}));
    assert.throws(()=>check({...app,adapter:'environment'}));
  }
});
