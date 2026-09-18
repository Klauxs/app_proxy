import * as fs from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { setupCore } from './setup-core.ts';
import { builtinDirectory, release } from '../src/builtin.ts';
await setupCore();
const root=resolve('release');await fs.mkdir(root,{recursive:true});
// Never remove an existing package; each build gets its own directory.
const stamp=new Date().toISOString().replace(/[:.]/g,'-');
const name='AppProxy-Windows-x64-'+stamp;const dest=join(root,name);await fs.mkdir(dest);
for(const dir of ['src','native','examples'])await fs.cp(resolve(dir),join(dest,dir),{recursive:true});
for(const file of ['package.json','README.md','App Proxy.cmd','TEST-RESULTS.md'])await fs.copyFile(file,join(dest,file));
await fs.mkdir(join(dest,'runtime'));await fs.copyFile(process.execPath,join(dest,'runtime/node.exe'));
await fs.cp(builtinDirectory,join(dest,'runtime/sing-box'),{recursive:true});
const license=join(dirname(process.execPath),'LICENSE');
try { await fs.copyFile(license,join(dest,'runtime/NODE-LICENSE.txt')); }
catch {
  const r=await fetch(`https://raw.githubusercontent.com/nodejs/node/${process.version}/LICENSE`,{signal:AbortSignal.timeout(30000)});
  if(!r.ok)throw new Error('Unable to obtain matching Node license');
  await fs.writeFile(join(dest,'runtime/NODE-LICENSE.txt'),await r.text());
}
const manifest={app:'0.2.0',node:process.version,arch:process.arch,singBox:release,createdAt:new Date().toISOString()};await fs.writeFile(join(dest,'release.json'),JSON.stringify(manifest,null,2));
const ps=join(process.env.SystemRoot||'C:\\Windows','System32/WindowsPowerShell/v1.0/powershell.exe');
const script=join(root,'zip-package.ps1');
await fs.writeFile(script,"param([string]$Source,[string]$Destination)\n$ErrorActionPreference='Stop'\nAdd-Type -AssemblyName System.IO.Compression.FileSystem\n[IO.Compression.ZipFile]::CreateFromDirectory($Source,$Destination)\n");
const zip=join(root,name+'.zip');await promisify(execFile)(ps,['-NoProfile','-NonInteractive','-File',script,dest,zip],{windowsHide:true,timeout:120000});
const digest=createHash('sha256').update(await fs.readFile(zip)).digest('hex');await fs.writeFile(zip+'.sha256',`${digest}  ${name}.zip\n`);
console.log(JSON.stringify({directory:dest,zip,sha256:digest},null,2));
