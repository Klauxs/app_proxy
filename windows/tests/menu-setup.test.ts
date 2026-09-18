import {test,type TestContext} from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {menu} from '../src/cli.ts';
import type {Service} from '../src/service.ts';
import type {Profile} from '../src/types.ts';

const profile:Profile={id:'p',name:'测试代理',kind:'managed',host:'127.0.0.1',port:19099,nodes:[]};
type Step=[RegExp,string];
const appSteps:Step[]=[[/应用名称/,'Test'],[/EXE 路径/,'C:\\Test.exe'],[/启动方式/,'0'],[/Chromium/,'n'],[/附加参数/,''],[/创建桌面快捷方式/,'n']];
const manualSteps:Step[]=[[/配置代理/,'2'],[/代理名称/,''],[/本地监听端口/,''],[/上游协议/,'http'],[/上游地址/,'127.0.0.1'],[/上游端口/,'19098'],[/用户名/,''],[/使用 TLS/,'n']];
async function run(t:TestContext,steps:Step[],options:{profiles?:Profile[];discovered?:boolean;failVerify?:boolean}={}) {
  const calls:string[]=[];const apps:any[]=[];const errors:string[]=[];
  const profiles=[...(options.profiles||[])];let verified=false;
  t.mock.method(console,'log',()=>{});
  t.mock.method(console,'error',(message:string)=>errors.push(message));
  const s={
    store:{read:async()=>({profiles})},
    discoverSingBox:async()=>{calls.push('discover');return {available:options.discovered?[{host:'127.0.0.1',port:19099,version:'fixture'}]:[]};},
    useSingBox:async()=>{calls.push('reuse');verified=true;return {...profile,kind:'sing-box'};},
    addManaged:async(name:string,port:number,nodes:unknown[],source:unknown)=>{calls.push('create');assert.ok(port>=18099);assert.ok(nodes.length);profiles.push({...profile});return {...profile};},
    prepareProfile:async(id:string)=>{calls.push('verify');assert.equal(id,'p');if(options.failVerify)throw new Error('fixture unreachable');verified=true;return profile;},
    addApp:async(data:any)=>{calls.push('app');assert.ok(data.profileId===undefined||verified,'must verify before registering');apps.push(data);return {...data,id:'a',guard:false};},
  } as unknown as Service;
  await menu(s,async(message:string)=>{
    const next=steps.shift();assert.ok(next,'unexpected prompt: '+message);
    assert.match(message,next[0]);return next[1];
  });
  assert.equal(steps.length,0,'all expected prompts consumed');
  return {calls,apps,errors};
}
test('empty setup creates and verifies a manual proxy then binds the app without selecting a profile again',async t=>{
  const result=await run(t,[[/请选择/,'1'],...manualSteps,...appSteps,[/请选择/,'0']]);
  assert.deepEqual(result.calls,['discover','create','verify','app']);
  assert.equal(result.apps[0].profileId,'p');assert.deepEqual(result.errors,[]);
});
test('detected usable service goes straight to app setup',async t=>{
  const result=await run(t,[[/请选择/,'1'],[/使用已有代理/,''],...appSteps,[/请选择/,'0']],{discovered:true});
  assert.deepEqual(result.calls,['discover','reuse','app']);assert.deepEqual(result.errors,[]);
});
test('a single saved proxy can be selected with Enter and is verified before adding',async t=>{
  const result=await run(t,[[/请选择/,'1'],[/选择应用要使用的代理/,''],...appSteps,[/请选择/,'0']],{profiles:[profile]});
  assert.deepEqual(result.calls,['verify','app']);assert.equal(result.apps[0].profileId,'p');
});
test('failed connectivity or cancelled setup never registers an app',async t=>{
  const failed=await run(t,[[/请选择/,'1'],...manualSteps,[/请选择/,'0']],{failVerify:true});
  assert.equal(failed.apps.length,0);assert.match(failed.errors[0],/联网验证失败/);
  const cancelled=await run(t,[[/请选择/,'1'],[/配置代理/,'0'],[/请选择/,'0']]);
  assert.equal(cancelled.apps.length,0);assert.deepEqual(cancelled.calls,['discover']);
});
test('direct mode requires an explicit choice and does not provision a proxy',async t=>{
  const result=await run(t,[[/请选择/,'1'],[/配置代理/,'d'],...appSteps,[/请选择/,'0']]);
  assert.deepEqual(result.calls,['discover','app']);assert.equal(result.apps[0].profileId,undefined);
});
test('standalone proxy setup continues into adding an app with the new binding',async t=>{
  const result=await run(t,[[/请选择/,'3'],...manualSteps,[/代理已就绪，现在添加应用/,'y'],...appSteps,[/请选择/,'0']]);
  assert.deepEqual(result.calls,['discover','create','verify','app']);assert.equal(result.apps[0].profileId,'p');
});
test('subscription setup downloads and selects nodes before verifying and continuing',async t=>{
  const server=http.createServer((_req,res)=>res.end('vless://00000000-0000-4000-8000-000000000001@example.com:443?security=tls#Fixture'));
  await new Promise<void>(resolve=>server.listen(0,'127.0.0.1',resolve));
  try {
    const address=server.address() as {port:number};
    const result=await run(t,[[/请选择/,'1'],[/配置代理/,''],[/代理名称/,''],[/本地监听端口/,''],[/订阅 URL/,`http://127.0.0.1:${address.port}/subscription`],[/国家\/地区编号/,'0'],[/节点编号/,'1'],...appSteps,[/请选择/,'0']]);
    assert.deepEqual(result.calls,['discover','create','verify','app']);assert.deepEqual(result.errors,[]);
  }finally{await new Promise<void>(resolve=>server.close(()=>resolve()));}
});
