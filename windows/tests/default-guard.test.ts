import {test} from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {Service} from '../src/service.ts';

async function fixture() {
  const root=await fs.mkdtemp(resolve('.test-data/default-guard-'));
  const s=await new Service(root).init();
  const state=await s.store.read();
  state.profiles.push({id:'proxy',name:'test',kind:'sing-box',host:'127.0.0.1',port:18099,nodes:[]});
  await s.store.save(state);
  s.guard.start=async()=>({pid:1,created:'fixture',path:process.execPath,args:[],parent:0,session:1,owned:true}); // No background processes/UAC.
  const realCall=s.native.call.bind(s.native);
  s.native.call=(async(op:string,data:any)=>{
    if(op.startsWith('lock-'))return realCall(op,data);
    assert.ok(['events-install','task-install'].includes(op));
    return {installed:true,current:true};
  }) as typeof s.native.call;
  async function executable(name:string) {const file=join(root,name);await fs.writeFile(file,'fixture');return file;}
  return {s,executable,realCall};
}

test('adding Codex/Claude originals and clones enables and persists Guard by default',async()=>{
  const {s,executable}=await fixture();
  try {
    for(const exeName of ['Codex.exe','CLAUDE.EXE']) {
      const exe=await executable(exeName);
      const app=await s.addApp({name:'自定义显示名',exe,adapter:'chromium',profileId:'proxy'});
      assert.equal(app.guard,true);
      assert.equal((await s.store.read()).apps.find(a=>a.id===app.id)?.guard,true);
    }
    for(const instance of ['codex','claude'] as const) {
      const app=await s.addApp({name:'分身',exe:await executable(instance+'-instance.exe'),instance,profileId:'proxy'});
      assert.equal(app.guard,true);
    }
  }finally{s.close();}
});

test('direct, environment-only, unrelated apps and explicit opt-out do not authorize Guard',async()=>{
  const {s,executable,realCall}=await fixture();
  s.native.call=(async(op:string,data:any)=>{if(op.startsWith('lock-'))return realCall(op,data);assert.fail('should not authorize or install tasks');}) as typeof s.native.call;
  try {
    for(const data of [
      {exe:await executable('Codex.exe'),adapter:'chromium' as const},
      {exe:await executable('Claude.exe'),adapter:'environment' as const,profileId:'proxy'},
      {exe:await executable('Other.exe'),adapter:'chromium' as const,profileId:'proxy'},
      {exe:await executable('codex.exe'),adapter:'chromium' as const,profileId:'proxy',guard:false},
    ]) {
      const app=await s.addApp({name:'Codex',...data});
      assert.equal(app.guard,false);
    }
    await assert.rejects(s.addApp({name:'bad',exe:process.execPath,guard:'yes' as any}),/guard 必须/);
  }finally{s.close();}
});

test('cancelled default Guard authorization leaves the new registration unprotected and reports it',async()=>{
  const {s,executable,realCall}=await fixture();
  s.native.call=(async(op:string,data:any)=>{if(op.startsWith('lock-'))return realCall(op,data);assert.equal(op,'events-install');throw new Error('UAC cancelled');}) as typeof s.native.call;
  try {
    await assert.rejects(s.addApp({name:'Claude',exe:await executable('Claude.exe'),adapter:'chromium',profileId:'proxy'}),/应用已添加.*Guard.*UAC cancelled/);
    const apps=(await s.store.read()).apps;
    assert.equal(apps.length,1);assert.equal(apps[0].guard,false);
  }finally{s.close();}
});
