import * as fs from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { atomic, exists, sleep, uid, Store } from './store.ts';
import { Native } from './native.ts';
import { quote } from './integration.ts';
import type { App } from './types.ts';
import { prepareApplicationRoot } from './msix-storage.ts';

const childScript = fileURLToPath(new URL('./msix-child.ts', import.meta.url));
export async function launchPackage(store: Store, native: Native, app: App, args: string[], env: NodeJS.ProcessEnv) {
  if (!app.package) throw new Error('MSIX 包登记缺失');
  const pendingPath = store.path('state', `msix-pending-${app.id}.json`);
  await store.assertSafe(pendingPath);
  if (await exists(pendingPath)) {
    const pending = JSON.parse(await fs.readFile(pendingPath,'utf8'));
    if (Date.now() <= pending.expiresAt + 10000) throw new Error('上次 MSIX 启动仍待确认，请稍后查看进程，不能重复启动');
    // Old helpers cannot launch after expiry. Never reuse their request paths.
    await fs.rm(pendingPath);
  }
  const sharedRoot = await prepareApplicationRoot(store,native,app);
  const statePath = join(sharedRoot,'state');
  await store.assertSafe(statePath); await fs.mkdir(statePath,{recursive:true});
  const requestPath = join(statePath, `msix-launch-${uid()}.json`);
  const resultPath = requestPath + '.result.json';
  const environment: Record<string,string> = {};
  for (const key of ['HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY','CODEX_HOME','CODEX_ELECTRON_USER_DATA_PATH','CLAUDE_CONFIG_DIR']) {
    environment[key] = env[key] || '';
  }
  const expiresAt = Date.now() + 20000;
  await atomic(requestPath, {exe:app.exe,cwd:app.cwd,args,environment,expiresAt});
  await atomic(pendingPath, {requestPath,expiresAt});
  let completed = false;
  try {
    await native.call('package-launch', {...app.package,node:process.execPath,arguments:[childScript,requestPath].map(quote).join(' ')});
    while (Date.now() < expiresAt + 2000) {
      if (await exists(resultPath)) {
        await store.assertSafe(resultPath);
        const result = JSON.parse(await fs.readFile(resultPath,'utf8')); completed = true;
        if (result.error) throw new Error(`MSIX 包内启动失败（${/^[A-Z_0-9]+$/.test(result.error)?result.error:'UNKNOWN'}）`);
        if (!Number.isInteger(result.pid) || result.pid <= 0) throw new Error('MSIX 启动回执无效');
        return result.pid as number;
      }
      await sleep(150);
    }
    throw new Error('MSIX 启动结果尚未确认，请稍后查看状态；不会自动重复启动');
  } finally {
    // On a timeout keep the receipt and deadline so delayed activation cannot duplicate a retry.
    if (completed) for (const path of [requestPath,resultPath,pendingPath]) {await store.assertSafe(path);await fs.rm(path,{force:true});}
  }
}
