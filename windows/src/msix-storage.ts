import * as fs from 'node:fs/promises';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { Store, exists } from './store.ts';
import type { App } from './types.ts';
import type { Native } from './native.ts';

// LocalState is shared with the package even when ordinary AppData is redirected.
export function applicationRoot(store: Store, app: App) {
  if (!app.package?.isolatedStorage) return store.root;
  if (!process.env.LOCALAPPDATA || !/^[A-Za-z0-9._-]+$/.test(app.package.familyName)) throw new Error('MSIX 数据目录无效');
  const scope = createHash('sha256').update(store.scopeRoot.toLowerCase()).digest('hex').slice(0,24);
  return join(process.env.LOCALAPPDATA, 'Packages', app.package.familyName, 'LocalState', 'AppProxy', scope);
}
export async function prepareApplicationRoot(store: Store, native: Native, app: App) {
  const root = applicationRoot(store,app);
  if (root === store.root) return root;
  await store.assertSafe(root);
  await fs.mkdir(root,{recursive:true});
  await store.assertSafe(root);
  const marker = join(root,'.app-proxy-owned');
  await store.assertSafe(marker);
  if (!await exists(marker)) {
    if ((await fs.readdir(root)).length) throw new Error('MSIX 分身目录非空且不属于 App Proxy');
    await native.call('protect',{path:root});
    await fs.writeFile(marker,'Windows App Proxy MSIX v1\n',{flag:'wx'});
  }
  if (await fs.readFile(marker,'utf8') !== 'Windows App Proxy MSIX v1\n') throw new Error('MSIX 分身目录归属标记不匹配');
  return root;
}
