import {test} from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {Native,sameIdentity} from '../src/native.ts';

test('native token ownership keeps creation-time checks and rejects a mismatched stop',async()=>{
  const native=new Native();
  const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{windowsHide:true,stdio:'ignore'});
  await new Promise<void>((resolve,reject)=>{child.once('spawn',resolve);child.once('error',reject)});
  try{
    const identity=await native.identity(child.pid!);assert.ok(identity?.owned);
    assert.equal(identity.path.toLowerCase(),process.execPath.toLowerCase());
    assert.ok((await native.processes([process.execPath])).some(p=>sameIdentity(identity,p)));
    await assert.rejects(()=>native.stop({...identity,created:'2000-01-01T00:00:00.0000000Z'}),/identity changed/i);
    assert.ok(sameIdentity(identity,await native.identity(child.pid!)),'mismatched identity must not terminate the live process');
    await native.stop(identity);assert.equal(await native.identity(child.pid!),undefined);
  }finally{if(child.exitCode===null)child.kill();native.close()}
});
