import {test} from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {spawn} from 'node:child_process';
import {atomic,exists,sleep} from '../src/store.ts';

test('atomic configuration replacement survives a brief Windows sharing lock without deleting the old file',async()=>{
  const root=await fs.mkdtemp(resolve('.test-data/atomic-'));const path=join(root,'config.json'),ready=join(root,'ready');
  await atomic(path,{version:1});
  const child=spawn(join(process.env.SystemRoot!,'System32/WindowsPowerShell/v1.0/powershell.exe'),['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',resolve('tests/hold-file.ps1'),'-Path',path,'-Ready',ready],{windowsHide:true,stdio:'ignore'});
  await new Promise<void>((resolve,reject)=>{child.once('spawn',resolve);child.once('error',reject)});
  try{
    const deadline=Date.now()+5000;while(!await exists(ready)){assert.ok(Date.now()<deadline);await sleep(10)}
    assert.deepEqual(JSON.parse(await fs.readFile(path,'utf8')),{version:1});
    await atomic(path,{version:2});
    assert.deepEqual(JSON.parse(await fs.readFile(path,'utf8')),{version:2});
    assert.equal((await fs.readdir(root)).some(name=>name.endsWith('.tmp')),false);
  }finally{if(child.exitCode===null)child.kill()}
});
