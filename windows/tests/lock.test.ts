import {test} from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {resolve} from 'node:path';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {Service} from '../src/service.ts';
import {sleep} from '../src/store.ts';

test('Windows releases a crashed holder and concurrent contenders remain exclusive',{timeout:15000},async()=>{
  const root=await fs.mkdtemp(resolve('.test-data/lock-'));
  const a=await new Service(root).init(),b=await new Service(root).init();
  const child=spawn(process.execPath,[resolve('tests/lock-holder.ts'),root],{windowsHide:true,stdio:['ignore','pipe','pipe']});
  try {
    await new Promise<void>((accept,reject)=>{child.stdout.on('data',chunk=>{if(String(chunk).includes('LOCKED'))accept();});child.once('error',reject);child.once('exit',()=>reject(new Error('holder exited early')));});
    assert.equal(await a.native.call('lock-acquire',{path:a.store.path('state/config.lock'),token:'contender'}),false);
    const exited=once(child,'exit');child.kill();await exited;
    let active=0,max=0,count=0;
    await Promise.all(Array.from({length:6},(_,i)=>(i%2?a:b).store.lock(async()=>{
      active++;max=Math.max(max,active);const next=count+1;await sleep(30);count=next;active--;
    })));
    assert.equal(max,1);assert.equal(count,6);
    const path=a.store.path('state/config.lock');
    assert.equal(await a.native.call('lock-acquire',{path,token:'owner'}),true);
    await assert.rejects(()=>a.native.call('lock-release',{path,token:'other'}),/ownership/);
    assert.equal(await b.native.call('lock-acquire',{path,token:'other'}),false);
    await a.native.call('lock-release',{path,token:'owner'});
  } finally {child.kill();a.close();b.close();}
});
