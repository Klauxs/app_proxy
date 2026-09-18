import * as fs from 'node:fs/promises';
import { resolve, join, dirname, relative, isAbsolute } from 'node:path';
import { randomUUID } from 'node:crypto';
import { homedir } from 'node:os';
import type { State, Runtime } from './types.ts';
import type { Native } from './native.ts';
export const sleep = (ms: number) => new Promise<void>(r => setTimeout(r, ms));
export const uid = () => randomUUID();
export async function exists(path: string) { try { await fs.access(path); return true; } catch { return false; } }
export async function atomic(path: string, value: unknown) {
  const temp = path + '.' + uid() + '.tmp';
  try {
    await fs.writeFile(temp, JSON.stringify(value, null, 2), { encoding: 'utf8', flag: 'wx' });
    // Windows readers/scanners can briefly deny replacement. Keep the old file intact.
    for (let attempt=0;;attempt++) {
      try { await fs.rename(temp,path); break; }
      catch(error:any) {
        if (process.platform !== 'win32' || !['EPERM','EACCES','EBUSY'].includes(error.code) || attempt >= 6) throw error;
        await sleep(25 * (attempt+1));
      }
    }
  }
  finally { await fs.rm(temp, { force: true }); }
}
export function redact(value: string) {
  return value.replace(/https?:\/\/[^\s"'<>]+/gi, '[URL]').replace(/(?:password|token|secret|uuid)\s*[:=]\s*[^,\s]+/gi, '[secret]');
}
export class Store {
  root: string;
  scopeRoot: string;
  private native?: Native;
  private defaultLocator?: string;
  constructor(root?: string) {
    this.root = resolve(root || process.env.APP_PROXY_HOME || join(process.env.LOCALAPPDATA || '.', 'AppProxy')); this.scopeRoot = this.root;
    if (!root && !process.env.APP_PROXY_HOME) this.defaultLocator = join(homedir(), '.app-proxy-home.json');
  }
  path(...parts: string[]) { const p = resolve(this.root, ...parts); const r = relative(this.root, p); if (r.startsWith('..') || isAbsolute(r)) throw new Error('路径超出工具目录'); return p; }
  async init(native: Native) {
    this.native = native;
    if (this.defaultLocator) {
      await this.assertSafe(this.defaultLocator);
      try {
        const locator = JSON.parse(await fs.readFile(this.defaultLocator,'utf8'));
        if (locator.version !== 1 || typeof locator.root !== 'string' || !isAbsolute(locator.root)) throw new Error('默认数据目录记录无效');
        this.root = resolve(locator.root); this.scopeRoot = this.root;
        if (!(await exists(this.path('.app-proxy-owned')))) throw new Error('默认数据目录已不存在，请通过 --home 指定已有数据目录');
      } catch (e: any) { if (e.code !== 'ENOENT') throw e; }
    }
    await fs.mkdir(this.root, { recursive: true });
    await this.assertSafe(this.root);
    if (!(await exists(this.path('.app-proxy-owned')))) {
      if ((await fs.readdir(this.root)).length) throw new Error('数据目录非空且不属于 App Proxy，请指定空目录');
      await native.call('protect', { path: this.root });
      await fs.writeFile(this.path('.app-proxy-owned'), 'Windows App Proxy v1\n', { flag: 'wx' });
    }
    if (await fs.readFile(this.path('.app-proxy-owned'), 'utf8') !== 'Windows App Proxy v1\n') throw new Error('数据目录归属标记不匹配');
    // Resolve a FILE, not the directory: MSIX may redirect only files in the directory.
    const anchor = await exists(this.path('manifest.json')) ? this.path('manifest.json') : this.path('.app-proxy-owned');
    await this.assertSafe(anchor);
    const physicalRoot = dirname(await native.call<string>('physical-file',{path:anchor}));
    await this.assertSafe(physicalRoot);
    if (physicalRoot.toLowerCase() !== this.root.toLowerCase()) {
      this.root = physicalRoot;
      if (await fs.readFile(this.path('.app-proxy-owned'),'utf8') !== 'Windows App Proxy v1\n') throw new Error('实际数据目录归属标记不匹配');
    }
    const scopeFile = this.path('.store-scope.json');
    await this.assertSafe(scopeFile);
    if (await exists(scopeFile)) {
      const metadata = JSON.parse(await fs.readFile(scopeFile,'utf8'));
      if (metadata.version !== 1 || typeof metadata.scopeRoot !== 'string' || !isAbsolute(metadata.scopeRoot)) throw new Error('数据目录标识无效');
      this.scopeRoot = resolve(metadata.scopeRoot);
    } else { await atomic(scopeFile,{version:1,scopeRoot:this.scopeRoot}); }
    for (const dir of ['config', 'logs', 'state', 'bin']) {
      await fs.mkdir(this.path(dir), { recursive: true }); await this.assertSafe(this.path(dir));
    }
    if (!(await exists(this.path('manifest.json'))) || !(await exists(this.path('state/runtime.json')))) await this.lock(async () => {
      if (!(await exists(this.path('manifest.json')))) await this.save({ schemaVersion: 1, profiles: [], apps: [], settings: { testUrl: 'https://www.gstatic.com/generate_204', exitUrl: 'https://api.ipify.org' }, shortcuts: [] });
      if (!(await exists(this.path('state/runtime.json')))) await this.runtimeSave({ launches: {} });
    });
    if (this.defaultLocator) {
      // User profile is shared by packaged and ordinary processes. Never replace a different store.
      try { await fs.writeFile(this.defaultLocator,JSON.stringify({version:1,root:this.root}),{flag:'wx'}); }
      catch (e: any) {
        if (e.code !== 'EEXIST') throw e;
        const locator = JSON.parse(await fs.readFile(this.defaultLocator,'utf8'));
        if (locator.root?.toLowerCase() !== this.root.toLowerCase()) throw new Error('默认数据目录由另一个入口初始化，请重新打开');
      }
    }
  }
  async assertSafe(path: string) {
    // Reject junctions/symlinks anywhere between the volume and the target.
    let current = resolve(path);
    while (true) {
      if (await exists(current)) { if ((await fs.lstat(current)).isSymbolicLink()) throw new Error('不允许重解析路径: ' + current); }
      const parent = dirname(current); if (parent === current) break; current = parent;
    }
  }
  async read(): Promise<State> {
    await this.assertSafe(this.path('manifest.json'));
    const s = JSON.parse(await fs.readFile(this.path('manifest.json'), 'utf8')) as State;
    validate(s); return s;
  }
  async save(s: State) { validate(s); await this.assertSafe(this.path('manifest.json')); await atomic(this.path('manifest.json'), s); }
  async runtime(): Promise<Runtime> {
    try { return JSON.parse(await fs.readFile(this.path('state/runtime.json'), 'utf8')); }
    catch (e: any) { if (e.code === 'ENOENT') return { launches: {} }; throw new Error('运行状态损坏，请先诊断，不能猜测进程归属'); }
  }
  async runtimeSave(r: Runtime) { await this.assertSafe(this.path('state/runtime.json')); await atomic(this.path('state/runtime.json'), r); }
  async lock<T>(fn: () => Promise<T>): Promise<T> {
    if (!this.native) throw new Error('数据目录尚未初始化');
    const path = this.path('state/config.lock'); const token = uid(); const until = Date.now() + 35000;
    await this.assertSafe(path);
    while (!(await this.native.call<boolean>('lock-acquire', {path,token}))) {
      if (Date.now() > until) throw new Error('另一个操作仍在运行；请稍后重试');
      await sleep(100);
    }
    try { return await fn(); }
    finally { await this.native.call('lock-release', {path,token}); }
  }
  async log(event: string, message: string) {
    const path = this.path('logs/events.log'); await this.assertSafe(path);
    if (await exists(path) && (await fs.stat(path)).size > 2 * 1024 * 1024) await fs.rename(path, this.path('logs/events.previous.log')).catch(() => {});
    await fs.appendFile(path, `${new Date().toISOString()} ${event} ${redact(message)}\n`);
  }
}
export function validate(s: State) {
  if (s.schemaVersion !== 1 || !Array.isArray(s.profiles) || !Array.isArray(s.apps)) throw new Error('不支持的配置结构');
  const ids = new Set<string>(); const ports = new Set<number>();
  for (const p of s.profiles) {
    if (!/^[\w-]+$/.test(p.id) || ids.has(p.id)) throw new Error('代理 ID 无效或重复'); ids.add(p.id);
    if (!['managed','sing-box'].includes(p.kind)) throw new Error('代理类型无效');
    port(p.port); if (!/^[a-z\d.:-]+$/i.test(p.host)) throw new Error('代理地址无效');
    if (!(p.host === '127.0.0.1' || (p.kind === 'sing-box' && p.host === '::1')) || ports.has(p.port)) throw new Error('代理端口须在回环地址且不重复'); ports.add(p.port);
    if (p.kind === 'managed' && (!Array.isArray(p.nodes) || !p.nodes.some(n => n.selected))) throw new Error('至少选择一个出口节点');
    if (p.kind === 'sing-box' && (!Array.isArray(p.nodes) || p.nodes.length || p.source)) throw new Error('复用服务不保存节点或订阅配置');
  }
  const appIds = new Set<string>(); const guarded = new Set<string>();
  for (const a of s.apps) {
    if (!/^[\w-]+$/.test(a.id) || appIds.has(a.id)) throw new Error('应用 ID 无效或重复'); appIds.add(a.id);
    if (!isAbsolute(a.exe) || !isAbsolute(a.cwd) || !Array.isArray(a.args) || a.args.some(x => typeof x !== 'string' || x.includes('\0'))) throw new Error('应用路径或参数无效');
    if (!['chromium','environment'].includes(a.adapter)) throw new Error('未知应用适配器');
    if (a.instance !== undefined && (!['codex','claude'].includes(a.instance) || a.adapter !== 'chromium')) throw new Error('未知分身类型');
    if (a.package && (!/^[A-Za-z0-9._-]+$/.test(a.package.familyName) || !/^[A-Za-z0-9._-]+$/.test(a.package.appId))) throw new Error('MSIX 包身份无效');
    if (a.package?.isolatedStorage !== undefined && typeof a.package.isolatedStorage !== 'boolean') throw new Error('MSIX 存储模式无效');
    if (a.profileId && !ids.has(a.profileId)) throw new Error('应用引用的代理不存在');
    if (a.guard) {
      if (a.adapter !== 'chromium' || !a.profileId) throw new Error('Guard 需要 Chromium 适配器和代理绑定');
      const key = a.exe.toLowerCase() + (a.instance ? ':' + a.id : ''); if (guarded.has(key)) throw new Error('同一实例只能有一个 Guard 绑定'); guarded.add(key);
    }
  }
  for (const url of [s.settings.testUrl, s.settings.exitUrl]) { if (!/^https?:\/\//.test(url)) throw new Error('探测 URL 需使用 HTTP/HTTPS'); }
}
export function port(value: unknown) { const n = Number(value); if (!Number.isInteger(n) || n < 1 || n > 65535) throw new Error('端口必须在 1–65535'); return n; }
