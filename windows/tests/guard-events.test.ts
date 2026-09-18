import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { basename, resolve } from 'node:path';
import { Service } from '../src/service.ts';
import { sleep } from '../src/store.ts';
import type { ProcessStart } from '../src/process-events.ts';
import type { Identity } from '../src/types.ts';

async function fixture() {
  const root = await fs.mkdtemp(resolve('.test-data/guard-events-'));
  const s = await new Service(root).init();
  const state = await s.store.read();
  state.profiles.push({id:'p',name:'p',kind:'managed',host:'127.0.0.1',port:18099,nodes:[{name:'fixture',protocol:'http',server:'127.0.0.1',server_port:18098,selected:true}]});
  state.apps.push({id:'app',name:'app',exe:process.execPath,cwd:root,args:[],adapter:'chromium',profileId:'p',guard:true});
  await s.store.save(state);
  return s;
}
async function until(check: () => boolean, timeout = 5000) {
  const end = Date.now() + timeout;
  while (!check()) { assert.ok(Date.now() < end, 'condition timed out'); await sleep(20); }
}

test('Guard never treats missing arguments as direct launch; a fresh readable process needs no grace', async () => {
  const s = await fixture();
  try {
    const target: Identity = {pid:123,parent:0,path:process.execPath,args:[],created:new Date().toISOString(),session:1,owned:true};
    let stopped = false;
    let launched = false;
    s.native.processes = async () => stopped ? [] : [target];
    s.native.stop = async () => { assert.ok(Date.now() - Date.parse(target.created) < 1200); stopped = true; };
    s.apps.launchUnlocked = async () => { launched = true; return {pid:456,running:true,proxy:undefined,evidence:'fixture'}; };
    const name = basename(target.path).toLowerCase();
    assert.ok((await s.guard.tick(new Set([name]))).has(name));
    assert.equal(stopped, false);
    target.args = [process.execPath]; target.created = new Date().toISOString();
    await s.guard.tick(new Set([name]));
    assert.ok(stopped && launched);
  } finally { s.close(); }
});

test('Guard waits only for trailing helpers and never relaunches over surviving processes', async t => {
  for (const scenario of ['empty','helper-exits','helper-stays','new-main']) await t.test(scenario, async () => {
    const s = await fixture();
    try {
      const target: Identity = {pid:123,parent:0,path:process.execPath,args:[process.execPath],created:new Date().toISOString(),session:1,owned:true};
      const helper = {...target,pid:124,parent:123,args:[process.execPath,'--type=utility']};
      const newMain = {...target,pid:125};
      let stopped = false, launched = false, cleanupChecks = 0;
      s.native.stop = async id => { assert.equal(id.pid,target.pid); stopped = true; };
      s.native.processes = async () => {
        if (!stopped) return [target];
        cleanupChecks++;
        if (scenario === 'helper-stays' || (scenario === 'helper-exits' && cleanupChecks === 1)) return [helper];
        return scenario === 'new-main' ? [newMain] : [];
      };
      s.apps.launchUnlocked = async (state,id,_timings,preflight) => {
        assert.equal(preflight?.app,state.apps.find(a=>a.id===id));
        assert.deepEqual(preflight?.processes,[]);
        launched = true; return {pid:456,running:true,proxy:undefined,evidence:'fixture'};
      };
      await s.guard.tick();
      assert.equal(launched,scenario === 'empty' || scenario === 'helper-exits');
      assert.equal(cleanupChecks,scenario === 'helper-stays' ? 4 : scenario === 'helper-exits' ? 2 : 1);
    } finally { s.close(); }
  });
});

test('reused launch preflight rejects a different app or a surviving target', async () => {
  const s = await fixture();
  try {
    const state = await s.store.read(), app = state.apps[0];
    s.apps.resolvePackage = async () => { throw new Error('unexpected second package lookup'); };
    await assert.rejects(()=>s.apps.launchUnlocked(state,app.id,undefined,{app:{...app},processes:[]}),/不匹配/);
    const target: Identity = {pid:123,parent:0,path:app.exe,args:[app.exe],created:new Date().toISOString(),session:1,owned:true};
    await assert.rejects(()=>s.apps.launchUnlocked(state,app.id,undefined,{app,processes:[target]}),/已经运行/);
  } finally { s.close(); }
});

test('Guard events wake immediately, coalesce, retry missing data finitely and do not overlap scans', {timeout:15000}, async () => {
  const s = await fixture();
  const abort = new AbortController();
  let run: Promise<void> | undefined;
  let start!: (event: ProcessStart) => void;
  let status!: (ready: boolean) => void;
  let closed = false;
  let active = 0;
  const calls: {names?: ReadonlySet<string>; at:number}[] = [];
  const name = basename(process.execPath).toLowerCase();
  try {
    s.guard.watchStarts = (event, state) => { start = event; status = state; return {close(){closed=true;}}; };
    s.guard.tick = async names => {
      assert.equal(++active, 1, 'scans must be serialized');
      calls.push({names,at:Date.now()}); await sleep(20); active--;
      return names ? new Set([name]) : new Set();
    };
    run = s.guard.run(abort.signal);
    await until(() => !!status && calls.length > 0 && active === 0);
    status(true);
    await until(() => calls.length >= 2 && active === 0);
    calls.length = 0;
    start({pid:1,name:'unregistered.exe'}); await sleep(100);
    assert.equal(calls.length, 0);
    const began = Date.now();
    for (let i=0;i<20;i++) start({pid:100+i,name});
    await until(() => calls.length === 4 && active === 0);
    assert.ok(calls[0].at - began < 1000, 'must not wait for the 30s fallback');
    assert.ok(calls.every(call => call.names?.has(name)));
    await sleep(400); assert.equal(calls.length, 4, 'one coalesced scan plus three bounded retries');
    abort.abort(); await run;
    assert.ok(closed); assert.equal((await s.store.runtime()).guard, undefined);
  } finally { abort.abort(); await run; s.close(); }
});

test('Guard listener failure restores 2s scans and configuration changes wake low-frequency mode', {timeout:15000}, async () => {
  const s = await fixture();
  const abort = new AbortController();
  let run: Promise<void> | undefined;
  let status!: (ready: boolean) => void;
  let count = 0;
  let subscriptions = 0;
  let closed = false;
  try {
    s.guard.watchStarts = (_event, state) => { subscriptions++; status=state; return {close(){closed=true;}}; };
    s.guard.tick = async () => { count++; return new Set(); };
    run = s.guard.run(abort.signal);
    await until(() => count > 0);
    status(true); await until(() => count >= 2);
    const before = count; await sleep(2200); assert.equal(count, before, 'healthy mode must not scan every 2s');
    status(false); await until(() => count > before);
    const failed = count;
    await until(() => count > failed, 4000);
    assert.ok(closed); assert.equal(subscriptions, 1, 'do not spin on listener failure');
    const state = await s.store.read(); state.apps[0].guard = false; await s.store.save(state);
    await run;
    assert.match(await fs.readFile(s.store.path('logs/events.log'),'utf8'), /guard-events-fallback/);
  } finally { abort.abort(); await run; s.close(); }
});

test('enabling Guard authorizes first, cancelled UAC preserves settings, and disabling never elevates', async () => {
  const s = await fixture();
  const calls: string[] = [];
  const original = s.native.call.bind(s.native);
  let permitted = false;
  try {
    const state = await s.store.read(); state.apps[0].guard = false; await s.store.save(state);
    s.native.call = (async (op: string, data: any) => {
      if (op === 'events-install') {
        calls.push(op); if (!permitted) throw new Error('UAC cancelled');
        return {installed:true,current:true};
      }
      if (op === 'task-install' || op === 'task-remove') { calls.push(op); return true; }
      return original(op,data);
    }) as typeof s.native.call;
    s.guard.start = async refresh => { assert.equal(refresh,true); calls.push('start'); return {pid:123,parent:0,path:process.execPath,args:[],created:'fixture',session:1,owned:true}; };
    await assert.rejects(()=>s.guard.enable('app',true),/UAC cancelled/);
    assert.equal((await s.store.read()).apps[0].guard,false); assert.deepEqual(calls,['events-install']);
    calls.length=0; permitted=true;
    await s.guard.enable('app',true);
    assert.deepEqual(calls,['events-install','task-install','start']);assert.equal((await s.store.read()).apps[0].guard,true);
    calls.length=0;await s.guard.enable('app',false);
    assert.deepEqual(calls,['task-remove']);assert.equal((await s.store.read()).apps[0].guard,false);
    calls.length=0;await assert.rejects(()=>s.guard.enable('missing',true),/不存在/);assert.deepEqual(calls,[]);
  } finally { s.close(); }
});
