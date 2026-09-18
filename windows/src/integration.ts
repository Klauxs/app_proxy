import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { createHash } from 'node:crypto';
import { Store } from './store.ts';
import { Native } from './native.ts';
import * as fs from 'node:fs/promises';
import type { PackageRegistration } from './types.ts';
export const cliPath = fileURLToPath(new URL('./cli.ts', import.meta.url));
export const hiddenPath = fileURLToPath(new URL('../native/hidden.vbs', import.meta.url));
export function quote(arg: string) { return '"' + arg.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\+)$/g, '$1$1') + '"'; }
export function launchSpec(store: Store, args: string[]) {
  return { target: join(process.env.SystemRoot || 'C:\\Windows', 'System32/wscript.exe'), arguments: [hiddenPath, process.execPath, cliPath, '--home', store.root, ...args].map(quote).join(' '), cwd: dirname(cliPath) };
}
export function taskSpec(store: Store) {
  return { name: 'AppProxy-Guard-' + createHash('sha256').update(store.scopeRoot.toLowerCase()).digest('hex').slice(0,12), description: 'App Proxy Guard: ' + store.scopeRoot, ...launchSpec(store, ['guard','run']) };
}
export async function shortcut(store: Store, native: Native, id: string, remove = false) {
  return store.lock(async () => {
    const state = await store.read(); const app = state.apps.find(a => a.id === id); if (!app) throw new Error('应用不存在');
    let item = state.shortcuts.find(s => s.appId === id);
    const spec = launchSpec(store, ['--notify', 'launch', id]);
    if (remove) {
      if (item) { await native.call('shortcut-remove', { path: item.path, ...spec }); state.shortcuts = state.shortcuts.filter(s => s.appId !== id); }
    } else {
      const desktop = await native.call<string>('desktop');
      let name = app.name.replace(/[<>:"/\\|?*\x00-\x1f]/g, '_').slice(0,100).replace(/[. ]+$/g, '') || '应用';
      if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(name)) name = '_' + name;
      const path = join(item ? dirname(item.path) : desktop, name + (app.instance ? ' - ' + id.slice(0,8) : '') + '.lnk');
      const renamed = !!item && item.path.toLowerCase() !== path.toLowerCase();
      // Never overwrite an unrelated shortcut that happens to use the same display name.
      if (!item || renamed) { try { await fs.access(path); throw new Error('同名快捷方式已存在，请先重命名或移走：' + path); } catch (e: any) { if (e.code !== 'ENOENT') throw e; } }
      let iconSource = app.exe;
      if (app.package) {
        const registration = await native.call<PackageRegistration|null>('package-resolve',app.package);
        if (!registration) throw new Error('未找到当前用户的 MSIX 应用登记');
        iconSource = registration.exe;
      }
      const iconDirectory = store.path('icons');
      await store.assertSafe(iconDirectory); await fs.mkdir(iconDirectory,{recursive:true});
      const iconPath = store.path('icons',id+'.ico');
      await store.assertSafe(iconPath);
      await native.call('shortcut', { path, ...spec, iconSource, iconPath });
      if (renamed) {
        try { await native.call('shortcut-remove', { path: item!.path, ...spec }); }
        catch (error) { await native.call('shortcut-remove', { path, ...spec }); throw error; }
      }
      if (item) item.path = path;
      else { item = { appId: id, path }; state.shortcuts.push(item); }
    }
    await store.save(state); return item?.path;
  });
}
