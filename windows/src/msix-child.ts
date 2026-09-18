// Runs once in the Windows package context. No credentials or arguments are logged.
import * as fs from 'node:fs/promises';
import { spawn } from 'node:child_process';
const requestPath = process.argv[2];
const resultPath = requestPath + '.result.json';
async function receipt(value: unknown) {
  await fs.writeFile(resultPath+'.tmp',JSON.stringify(value),{flag:'wx'});
  await fs.rename(resultPath+'.tmp',resultPath);
}
try {
  const request = JSON.parse(await fs.readFile(requestPath, 'utf8'));
  if (!Number.isFinite(request.expiresAt) || Date.now() > request.expiresAt) throw Object.assign(new Error(),{code:'LAUNCH_EXPIRED'});
  const env = { ...process.env };
  const overrides = new Map<string,string>(Object.entries(request.environment));
  for (const key of Object.keys(env)) if (overrides.has(key.toUpperCase())) delete env[key];
  for (const [key,value] of overrides) env[key] = value;
  const child = spawn(request.exe, request.args, { cwd: request.cwd, env, detached: true, windowsHide: false, stdio: 'ignore' });
  await new Promise<void>((resolve,reject) => {child.once('spawn',resolve);child.once('error',reject);});
  child.unref();
  await receipt({pid:child.pid});
} catch (error: any) {
  await receipt({error:/^[A-Z_0-9]+$/.test(error.code || '') ? error.code : 'LAUNCH_FAILED'});
}
