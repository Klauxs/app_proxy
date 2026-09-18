// Opt-in interactive test: node tests/elevated-events-live.ts (one Windows UAC prompt).
import * as fs from 'node:fs/promises';
import {resolve,join,basename} from 'node:path';
import {randomBytes} from 'node:crypto';
import {spawn,execFile} from 'node:child_process';
import {promisify} from 'node:util';
import assert from 'node:assert/strict';
import {Native} from '../src/native.ts';
import {watchGuardEvents} from '../src/elevated-events.ts';
import {sleep,exists} from '../src/store.ts';
import type {ProcessStart,ProcessWatcher} from '../src/process-events.ts';
const fixture=await fs.mkdtemp(resolve('.test-data/events-live-'));
const key=process.argv[2]||randomBytes(6).toString('hex');assert.match(key,/^[a-f0-9]{12}$/);
const ps=join(process.env.SystemRoot!,'System32/WindowsPowerShell/v1.0/powershell.exe');
const admin=resolve('tests/elevated-events-admin.ps1');
const quote=(value:string)=>"'"+value.replaceAll("'","''")+"'";
const argumentsText=`-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "${admin}" -Key ${key} -Fixture "${fixture}"`;
const command=`$ErrorActionPreference='Stop'; $p=Start-Process -FilePath ${quote(ps)} -ArgumentList ${quote(argumentsText)} -Verb RunAs -WindowStyle Hidden -PassThru; $p.WaitForExit(); exit $p.ExitCode`;
const consent=promisify(execFile)(ps,['-NoProfile','-NonInteractive','-Command',command],{windowsHide:true,timeout:240000}).then(()=>undefined,error=>error);
const native=new Native();let watcher:ProcessWatcher|undefined;let child:ReturnType<typeof spawn>|undefined;
const received=new Map<number,ProcessStart & {receivedAt:number}>();
const children:ReturnType<typeof spawn>[]=[];
const timings:object[]=[];
async function timing(pid:number,began:number) {
  const event=received.get(pid)!;
  assert.equal(event.name.toLowerCase(),basename(process.execPath).toLowerCase());
  const identity=await native.identity(pid);assert.ok(identity?.owned);
  const created=Date.parse(identity.created);
  assert.equal(typeof event.callbackAt,'number');assert.equal(typeof event.eventAt,'number');
  timings.push({pid,spawnToCreatedMs:created-began,createdToEventMs:event.eventAt!-created,eventToCallbackMs:event.callbackAt!-event.eventAt!,callbackToClientMs:event.receivedAt-event.callbackAt!,totalMs:event.receivedAt-began,
    createdAt:created,eventAt:event.eventAt,callbackAt:event.callbackAt,receivedAt:event.receivedAt});
}
async function until(check:()=>Promise<boolean>,timeout=20000){const end=Date.now()+timeout;while(!await check()){assert.ok(Date.now()<end,'condition timed out');await sleep(20);}}
try{
  await until(async()=>await exists(join(fixture,'ready'))||await exists(join(fixture,'cleanup.json')),120000);
  assert.ok(await exists(join(fixture,'ready')),'elevated setup failed');
  const status=await native.call('events-status',{key});assert.ok(status.installed&&status.current);
  assert.ok((await native.call('events-install',{key})).current,'unchanged installation must reuse authorization');
  const detailsCommand=`. ${quote(resolve('native/events-task.ps1'))}; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $spec=Get-EventsSpec '${key}' $sid; $task=Get-EventsTask $spec; $principal=New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent()); @{sid=$sid; script=$spec.script; elevated=$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator); taskSddl=$task.GetSecurityDescriptor(7)} | ConvertTo-Json`;
  const details=JSON.parse((await promisify(execFile)(ps,['-NoProfile','-NonInteractive','-Command',detailsCommand],{windowsHide:true})).stdout);
  assert.equal(details.elevated,false,'test client must remain unelevated');assert.match(details.taskSddl,/^O:BA/);
  const ace=details.taskSddl.split('(').find((part:string)=>part.includes(';;;'+details.sid+')'));
  assert.ok(ace?.startsWith('A;;0x1200a9;;;'),'ordinary user gets read/execute only');
  let writable=false;try{const handle=await fs.open(details.script,'r+');await handle.close();writable=true;}catch(error:any){assert.ok(['EPERM','EACCES'].includes(error.code));}
  assert.equal(writable,false,'ordinary user must not rewrite the elevated payload');
  let ready=false;let failure='';let seen=false;let latency=0;let began=0;
  watcher=watchGuardEvents(native,key,event=>{received.set(event.pid,{...event,receivedAt:Date.now()});if(event.pid===child?.pid){seen=true;latency=Date.now()-began;}},(state,reason)=>{if(state){ready=true;assert.equal(reason,'elevated-etw-process-start');}else failure=reason!;});
  await until(async()=>ready||!!failure);assert.ok(ready,failure);
  began=Date.now();child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore',windowsHide:true});
  await new Promise<void>((resolve,reject)=>{child!.once('spawn',resolve);child!.once('error',reject);});
  await until(async()=>seen);assert.ok((await native.identity(child.pid!))?.owned);
  await timing(child.pid!,began);
  const firstLatency=latency;
  child.kill();watcher.close();await until(async()=>!(await native.call('events-status',{key})).running);
  ready=false;seen=false;failure='';
  watcher=watchGuardEvents(native,key,event=>{received.set(event.pid,{...event,receivedAt:Date.now()});if(event.pid===child?.pid){seen=true;latency=Date.now()-began;}},(state,reason)=>{if(state)ready=true;else failure=reason!;});
  await until(async()=>ready||!!failure);assert.ok(ready,failure);
  began=Date.now();child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore',windowsHide:true});
  await new Promise<void>((resolve,reject)=>{child!.once('spawn',resolve);child!.once('error',reject);});
  await until(async()=>seen);
  await timing(child.pid!,began);
  const starts=new Map<number,number>();
  for(const delay of [0,130,270,310,470,190]) {
    await sleep(delay);const start=Date.now();
    const item=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore',windowsHide:true});children.push(item);
    await new Promise<void>((resolve,reject)=>{item.once('spawn',resolve);item.once('error',reject);});starts.set(item.pid!,start);
  }
  await until(async()=>children.every(item=>received.has(item.pid!)));
  for(const item of children) await timing(item.pid!,starts.get(item.pid!)!);
  const burst:number[]=[];
  await Promise.all(Array.from({length:32},async()=>{
    const item=spawn(process.execPath,['-e','setTimeout(()=>{},50)'],{stdio:'ignore',windowsHide:true});children.push(item);
    await new Promise<void>((resolve,reject)=>{item.once('spawn',resolve);item.once('error',reject);});burst.push(item.pid!);
    await new Promise<void>(resolve=>item.once('exit',()=>resolve()));
  }));
  await until(async()=>burst.every(pid=>received.has(pid)));
  for(const pid of burst)assert.equal(received.get(pid)!.name.toLowerCase(),basename(process.execPath).toLowerCase());
  await fs.writeFile(join(fixture,'measure'),'measure');await until(async()=>exists(join(fixture,'metrics.json')));
  const idle=JSON.parse((await fs.readFile(join(fixture,'metrics.json'),'utf8')).replace(/^\uFEFF/,''));
  await fs.writeFile(join(fixture,'crash'),'crash');await until(async()=>exists(join(fixture,'crashed')));
  await until(async()=>!!failure);watcher.close();await until(async()=>!(await native.call('events-status',{key})).running);
  ready=false;seen=false;failure='';child.kill();
  watcher=watchGuardEvents(native,key,event=>{received.set(event.pid,{...event,receivedAt:Date.now()});if(event.pid===child?.pid)seen=true;},(state,reason)=>{if(state)ready=true;else failure=reason!;});
  await until(async()=>ready||!!failure);assert.ok(ready,failure);
  child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore',windowsHide:true});
  await until(async()=>seen||!!failure);assert.ok(seen,failure);
  assert.equal(failure,'');
  const evidence={realElevatedEvents:true,source:'etw',flushMs:100,notificationMs:[firstLatency,latency],timings,burstReceived:burst.length,idle,orphanRecovery:true,unelevatedClient:true,protectedPayload:true,taskReadExecuteOnly:true,restartWithoutUac:true,fixture,key};
  await fs.writeFile(join(fixture,'evidence.json'),JSON.stringify(evidence,null,2));console.log(JSON.stringify(evidence));
}finally{
  child?.kill();for(const item of children)if(item.exitCode===null)item.kill();watcher?.close();await fs.writeFile(join(fixture,'done'),'done');
  const error=await consent;
  try{if(error)throw error;const cleanup=JSON.parse((await fs.readFile(join(fixture,'cleanup.json'),'utf8')).replace(/^\uFEFF/,''));assert.equal(cleanup.error,null);assert.equal(cleanup.removeExit,0);assert.equal(cleanup.traceRemoved,true);assert.equal((await native.call('events-status',{key})).installed,false);console.log('Unique elevated test task and ETW session removed.');}
  finally{native.close();}
}
