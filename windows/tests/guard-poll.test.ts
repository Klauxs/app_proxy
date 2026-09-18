import {test} from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {resolve} from 'node:path';
import {Service} from '../src/service.ts';
import type {Identity} from '../src/types.ts';

test('Guard polling leaves launcher lock free and discards a snapshot changed while scanning',{timeout:10000},async()=>{
  const root=await fs.mkdtemp(resolve('.test-data/guard-poll-'));
  const s=await new Service(root).init();
  function deferred(){let resolve!:()=>void;const promise=new Promise<void>(r=>{resolve=r});return {promise,resolve}}
  const entered=deferred();const resume=deferred();
  let stopped=false;let tick:Promise<Set<string>>|undefined;
  try{
    const state=await s.store.read();
    state.profiles.push({id:'profile',name:'profile',kind:'managed',host:'127.0.0.1',port:18099,nodes:[{name:'fixture',protocol:'http',server:'127.0.0.1',server_port:18098,selected:true}]});
    state.apps.push({id:'app',name:'app',exe:process.execPath,cwd:root,args:[],adapter:'chromium',profileId:'profile',guard:true});
    await s.store.save(state);
    const target:Identity={pid:123,parent:0,path:process.execPath,args:[process.execPath],created:'2000-01-01T00:00:00Z',session:1,owned:true};
    s.native.processes=async()=>{entered.resolve();await resume.promise;return [target]};
    s.native.stop=async()=>{stopped=true};
    tick=s.guard.tick();await entered.promise;
    // This must finish before the poll resumes. The prior implementation held the lock here.
    let timer:NodeJS.Timeout|undefined;
    try{await Promise.race([s.store.lock(async()=>{const next=await s.store.read();next.apps[0].guard=false;await s.store.save(next)}),new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('poll blocked launcher lock')),3000)})])}finally{clearTimeout(timer)}
    resume.resolve();await tick;
    assert.equal(stopped,false);assert.equal((await s.store.read()).apps[0].guard,false);
  }finally{resume.resolve();await tick;s.close()}
});

test('a removed MSIX app does not prevent Guard correcting another app',async()=>{
  const root=await fs.mkdtemp(resolve('.test-data/guard-unavailable-'));
  const s=await new Service(root).init();
  try {
    const state=await s.store.read();
    state.profiles.push({id:'p',name:'p',kind:'managed',host:'127.0.0.1',port:18099,nodes:[{name:'fixture',protocol:'http',server:'127.0.0.1',server_port:18098,selected:true}]});
    state.apps.push({id:'removed',name:'removed',exe:'C:\\Program Files\\WindowsApps\\Removed_fixture\\app.exe',cwd:root,args:[],adapter:'chromium',guard:true,profileId:'p',package:{familyName:'Removed_fixture',appId:'App'}},
      {id:'healthy',name:'healthy',exe:process.execPath,cwd:root,args:[],adapter:'chromium',guard:true,profileId:'p'});
    await s.store.save(state);
    const call=s.native.call.bind(s.native);
    s.native.call=(async(op:string,data:any)=>op==='package-resolve'?null:call(op,data)) as typeof s.native.call;
    const target:Identity={pid:123,parent:0,path:process.execPath,args:[process.execPath],created:'2000-01-01T00:00:00Z',session:1,owned:true};
    let stopped=false;let launched='';
    s.native.processes=async()=>stopped?[]:[target];
    s.native.stop=async()=>{stopped=true;};
    s.apps.launchUnlocked=async(_state,id)=>{launched=id;return {pid:456,running:true,proxy:undefined,evidence:'fixture'};};
    await s.guard.tick();
    assert.equal(stopped,true);assert.equal(launched,'healthy');
    assert.match(await fs.readFile(s.store.path('logs/events.log'),'utf8'),/guard-unavailable removed/);
  } finally {s.close();}
});
