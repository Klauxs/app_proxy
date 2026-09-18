import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createServer, type Socket} from 'node:net';
import {randomUUID} from 'node:crypto';
import {Native} from '../src/native.ts';
import {watchGuardEvents} from '../src/elevated-events.ts';
import {sleep} from '../src/store.ts';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {join,resolve} from 'node:path';

test('self-contained ETW payload compiles under system PowerShell 5.1',async()=>{
  const result=await promisify(execFile)(join(process.env.SystemRoot!,'System32/WindowsPowerShell/v1.0/powershell.exe'),
    ['-NoLogo','-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',resolve('tests/elevated-events-compile.ps1')],{windowsHide:true});
  assert.match(result.stdout,/ETW payload compiled/);
});

async function until(check:()=>boolean) {
  const end=Date.now()+5000;
  while(!check()){assert.ok(Date.now()<end,'condition timed out');await sleep(10);}
}
test('elevated notification channel handles split messages and disconnects without sending commands',async()=>{
  const pipe='\\\\.\\pipe\\app-proxy-test-'+randomUUID();
  let client:Socket|undefined;let bytes=0;let ended=false;
  const server=createServer(socket=>{client=socket;socket.on('data',chunk=>{bytes+=chunk.length});socket.on('close',()=>{ended=true});});
  await new Promise<void>(resolve=>server.listen(pipe,resolve));
  const native=new Native();const calls:string[]=[];
  native.call=(async(op:string)=>{calls.push(op);return {installed:true,current:true,running:true,pipe};}) as typeof native.call;
  let ready=false;const events:{pid:number;name:string}[]=[];
  const watcher=watchGuardEvents(native,'abcdef123456',event=>events.push(event),(state,reason)=>{ready=state;assert.equal(reason,'elevated-etw-process-start');});
  try {
    await until(()=>!!client);
    client!.write('{"type":"rea');client!.write('dy","elevated":true,"source":"etw"}\n{"type":"start","pid":123,"name":"fixture.exe"}\n');
    await until(()=>ready&&events.length===1);
    assert.deepEqual(events,[{pid:123,name:'fixture.exe'}]);
    assert.deepEqual(calls,['events-status','events-start']);
    watcher.close();await until(()=>ended);assert.equal(bytes,0,'ordinary Guard must send no privileged commands');
  } finally {watcher.close();client?.destroy();await new Promise<void>(resolve=>server.close(()=>resolve()));}
});
test('an installed listener needing an update falls back without prompting for elevation',async()=>{
  const native=new Native();const calls:string[]=[];
  native.call=(async(op:string)=>{calls.push(op);return {installed:true,current:false};}) as typeof native.call;
  let failure='';
  const watcher=watchGuardEvents(native,'abcdef123456',()=>assert.fail('unexpected event'),(ready,reason)=>{assert.equal(ready,false);failure=reason!;});
  try {await until(()=>!!failure);assert.equal(failure,'listener-update-requires-authorization');assert.deepEqual(calls,['events-status']);}
  finally{watcher.close();}
});
test('ETW loss or decode failure closes the channel and preserves its diagnostic reason',async()=>{
  const pipe='\\\\.\\pipe\\app-proxy-test-'+randomUUID();
  let client:Socket|undefined;
  const server=createServer(socket=>{client=socket;});
  await new Promise<void>(resolve=>server.listen(pipe,resolve));
  const native=new Native();
  native.call=(async()=>({installed:true,current:true,running:true,pipe})) as typeof native.call;
  let failure='';
  const watcher=watchGuardEvents(native,'abcdef123456',()=>assert.fail('unexpected start'),(ready,reason)=>{if(!ready)failure=reason!;});
  try {
    await until(()=>!!client);
    client!.write('{"type":"ready","elevated":true,"source":"etw"}\n{"type":"error","reason":"etw-events-lost"}\n');
    await until(()=>!!failure);
    assert.equal(failure,'etw-events-lost');
    await until(()=>!!client?.destroyed);
  }finally{watcher.close();client?.destroy();await new Promise<void>(resolve=>server.close(()=>resolve()));}
});
test('missing authorization reports an error without using a non-elevated listener or background UAC',async()=>{
  const native=new Native();const calls:string[]=[];
  native.call=(async(op:string)=>{calls.push(op);return {installed:false,current:false};}) as typeof native.call;
  let failure='';
  const watcher=watchGuardEvents(native,'abcdef123456',()=>assert.fail('unexpected event'),(ready,reason)=>{assert.equal(ready,false);failure=reason!;});
  try{await until(()=>!!failure);assert.equal(failure,'listener-authorization-required');assert.deepEqual(calls,['events-status']);}
  finally{watcher.close();}
});
test('closing the elevated channel cancels connection retries without callbacks',async()=>{
  const native=new Native();let calls=0;let states=0;
  native.call=(async()=>{calls++;return {installed:true,current:true,pipe:'\\\\.\\pipe\\app-proxy-missing-'+randomUUID()};}) as typeof native.call;
  const watcher=watchGuardEvents(native,'abcdef123456',()=>assert.fail('unexpected event'),()=>{states++;});
  await until(()=>calls===2);await sleep(100);watcher.close();await sleep(250);
  assert.equal(states,0);
});
