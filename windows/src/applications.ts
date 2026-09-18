import * as fs from 'node:fs/promises';
import { dirname, resolve, join, win32 } from 'node:path';
import { spawn } from 'node:child_process';
import { Store, sleep, uid } from './store.ts';
import { Native, sameIdentity } from './native.ts';
import { Core } from './core.ts';
import { probe, proxyUrl } from './proxy.ts';
import type { App, State, Identity, PackageRegistration } from './types.ts';
import { taskSpec } from './integration.ts';
import { launchPackage } from './msix.ts';
import { applicationRoot, prepareApplicationRoot } from './msix-storage.ts';
import { verifyListener } from './singbox.ts';
export function defaultGuard(app: Pick<App,'exe'|'adapter'|'profileId'|'instance'|'package'>) {
  // Match the executable, not its editable display name. Environment-only CLI tools stay opt-in.
  const packagedDesktop = (app.package?.familyName === 'OpenAI.Codex_2p2nqsd0c76g0' && app.package.appId === 'App') ||
    (app.package?.familyName === 'Claude_pzs8sxrjxfjjc' && app.package.appId === 'Claude');
  return app.adapter === 'chromium' && !!app.profileId &&
    (!!app.instance || packagedDesktop || /^(codex|claude)\.exe$/i.test(win32.basename(app.exe)));
}
export function childEnvironment(proxy?: string) {
  const env: NodeJS.ProcessEnv = {};
  const seen = new Set<string>();
  for (const [key, value] of Object.entries(process.env)) {
    const normalized = key.toUpperCase();
    if (['HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY'].includes(normalized) || seen.has(normalized)) continue;
    seen.add(normalized); env[key] = value;
  }
  if (proxy) { env.HTTP_PROXY = proxy; env.HTTPS_PROXY = proxy; env.ALL_PROXY = proxy; env.NO_PROXY = ''; }
  return env;
}
export function isHelper(id: Identity) { return id.args.some(a => /^--type(?:=|$)/.test(a) || /^--crashpad-handler(?:=|$)/.test(a)); }
export function argumentValues(args: string[], name: string) {
  const result: string[] = [];
  for (let i = 0; i < args.length; i++) {
    if (args[i] === name) result.push(args[++i] || '');
    else if (args[i].startsWith(name + '=')) result.push(args[i].slice(name.length + 1));
  }
  return result;
}
export function instancePaths(store: Store, app: App) {
  const root = applicationRoot(store,app);
  return { userData: join(root, 'instances', app.id, 'user-data'), home: join(root, 'instances', app.id, app.instance === 'claude' ? 'claude-home' : 'codex-home') };
}
export function appProcesses(store: Store, app: App, processes: Identity[]) {
  const expected = app.instance ? instancePaths(store, app).userData : argumentValues(app.args, '--user-data-dir')[0];
  // Previously launched clones can still carry the logical path that MSIX redirected.
  const aliases = expected ? [expected] : [];
  if (app.instance && !app.package?.isolatedStorage && store.scopeRoot.toLowerCase() !== store.root.toLowerCase()) aliases.push(join(store.scopeRoot,'instances',app.id,'user-data'));
  const candidates = processes.filter(p => p.path.toLowerCase() === app.exe.toLowerCase());
  const byPid = new Map(candidates.map(p => [p.pid, p]));
  return candidates.filter(p => {
    let values = argumentValues(p.args, '--user-data-dir');
    // Helpers can omit the argument; inherit only from an observed same-EXE ancestor.
    const visited = new Set<number>([p.pid]); let parent = byPid.get(p.parent);
    while (!values.length && isHelper(p) && parent && !visited.has(parent.pid)) {
      visited.add(parent.pid); values = argumentValues(parent.args, '--user-data-dir'); parent = byPid.get(parent.parent);
    }
    return expected ? values.length === 1 && !!values[0] && aliases.some(path=>resolve(values[0]).toLowerCase() === resolve(path).toLowerCase()) : values.length === 0;
  });
}
export function expectedProxy(id: Identity, proxy: string) {
  if (id.args.some(a => /^--(?:no-proxy-server|proxy-pac-url|proxy-bypass-list)(?:=|$)/.test(a))) return false;
  const values: string[] = [];
  for (let i = 0; i < id.args.length; i++) {
    const a = id.args[i]; if (a === '--proxy-server') values.push(id.args[++i] || '');
    else if (a.startsWith('--proxy-server=')) values.push(a.slice('--proxy-server='.length));
  }
  return values.length === 1 && values[0] === proxy;
}
export class Applications {
  readonly store: Store; readonly native: Native; readonly core: Core;
  constructor(store: Store, native: Native, core: Core) { this.store = store; this.native = native; this.core = core; }
  async desktop(kind: 'codex'|'claude') {
    if (!['codex','claude'].includes(kind)) throw new Error('仅支持 Codex 或 Claude 桌面应用');
    const name=kind==='codex'?'Codex':'Claude';
    const registration=await this.native.call<PackageRegistration|null>('package-resolve',{desktop:kind});
    if (!registration?.fullTrust || !registration.exe) throw new Error(`未找到当前用户安装的 ${name} 桌面版；请先安装，或选择“其他应用”手动指定 EXE`);
    return {name,exe:registration.exe,adapter:'chromium' as const,args:[] as string[]};
  }
  async add(data: { name: string; exe: string; cwd?: string; args?: string[]; adapter?: App['adapter']; profileId?: string; instance?: App['instance'] }) {
    const exe = await fs.realpath(resolve(data.exe)); if (!exe.toLowerCase().endsWith('.exe')) throw new Error('请选择实际 EXE，暂不支持 .cmd');
    const cwd = await fs.realpath(resolve(data.cwd || dirname(exe))); if (!(await fs.stat(cwd)).isDirectory()) throw new Error('工作目录无效');
    const app: App = { id: uid(), name: data.name, exe, cwd, args: data.args || [], adapter: data.instance ? 'chromium' : data.adapter || 'environment', profileId: data.profileId, guard: false, instance: data.instance };
    this.checkArgs(app);
    await this.resolvePackage(app);
    return this.store.lock(async () => { const s = await this.store.read(); if (!app.instance && s.apps.some(a => !a.instance && a.guard && a.exe.toLowerCase() === exe.toLowerCase())) throw new Error('该 EXE 已由 Guard 保护，请修改已有登记'); s.apps.push(app); await this.store.save(s); return app; });
  }
  async resolvePackage(app: App) {
    if (!app.package && !/[\\/]WindowsApps[\\/]/i.test(app.exe)) return false;
    const registration = await this.native.call<PackageRegistration|null>('package-resolve',app.package || {exe:app.exe});
    if (!registration) throw new Error('未找到当前用户的 MSIX 应用登记，请确认应用仍已安装');
    if (!registration.fullTrust) throw new Error('MSIX 适配目前仅支持 full-trust 桌面应用');
    const previous = JSON.stringify({exe:app.exe,cwd:app.cwd,package:app.package});
    if (app.cwd.toLowerCase() === dirname(app.exe).toLowerCase()) app.cwd = dirname(registration.exe);
    app.exe = registration.exe;
    // Once allocated, retain this location across package updates.
    const isolatedStorage = app.package?.isolatedStorage || registration.isolatedStorage;
    app.package = {familyName:registration.familyName,appId:registration.appId,...(isolatedStorage?{isolatedStorage:true}:{})};
    return previous !== JSON.stringify({exe:app.exe,cwd:app.cwd,package:app.package});
  }
  checkArgs(app: App) {
    if (app.instance && (!['codex','claude'].includes(app.instance) || app.adapter !== 'chromium' || app.args.some(a => /^--user-data-dir(?:=|$)/.test(a)))) throw new Error('分身数据目录由工具管理，不能覆盖 --user-data-dir');
    if (app.adapter === 'chromium' && app.args.some(a => a === '--' || /^--(?:proxy-server|proxy-pac-url|no-proxy-server|proxy-bypass-list)(?:=|$)/.test(a))) throw new Error('Chromium 参数含代理冲突或 -- 分隔符；请通过代理绑定设置');
  }
  async launch(id: string) { return this.store.lock(async () => this.launchUnlocked((await this.store.read()), id)); }
  async launchUnlocked(state: State, id: string, timings?: Record<string, number>, preflight?: { app: App; processes: Identity[] }) {
    let stageAt = performance.now();
    const mark = (stage: string) => { const now = performance.now(); if (timings) timings[stage] = Math.round(now - stageAt); stageAt = now; };
    const app = state.apps.find(a => a.id === id); if (!app) throw new Error('应用不存在');
    // Guard supplies its just-completed checks for this exact object under the same store lock.
    if (preflight && preflight.app !== app) throw new Error('启动检查与目标应用不匹配');
    if (!preflight && await this.resolvePackage(app)) await this.store.save(state);
    mark('resolveMs');
    this.checkArgs(app);
    const live = appProcesses(this.store, app, preflight ? preflight.processes : await this.native.processes([app.exe]));
    if (live.length) throw new Error('目标应用或辅助进程已经运行；请先关闭，不能向旧进程重新注入代理。Guard 会独立纠正已启用保护的误启动');
    mark('processCheckMs');
    const profile = state.profiles.find(p => p.id === app.profileId);
    if (app.profileId && !profile) throw new Error('代理绑定无效');
    if (profile?.kind === 'managed') { await this.core.startUnlocked(state, profile.id); await probe(profile, state.settings.testUrl); }
    else if (profile) await verifyListener(this.native,profile,state.settings.testUrl);
    mark('proxyReadyMs');
    const proxy = profile ? proxyUrl(profile) : undefined;
    const args = [...app.args];
    const env = childEnvironment(proxy);
    if (app.instance) {
      await prepareApplicationRoot(this.store,this.native,app);
      const paths = instancePaths(this.store, app);
      for (const path of Object.values(paths)) { await this.store.assertSafe(path); await fs.mkdir(path, { recursive: true }); await this.store.assertSafe(path); }
      for (const key of Object.keys(env)) if (['CODEX_HOME','CODEX_ELECTRON_USER_DATA_PATH','CLAUDE_CONFIG_DIR'].includes(key.toUpperCase())) delete env[key];
      if (app.instance === 'codex') { env.CODEX_HOME = paths.home; env.CODEX_ELECTRON_USER_DATA_PATH = paths.userData; }
      else env.CLAUDE_CONFIG_DIR = paths.home;
      args.push(`--user-data-dir=${paths.userData}`);
    }
    if (app.adapter === 'chromium') args.push(proxy ? `--proxy-server=${proxy}` : '--no-proxy-server');
    let pid: number;
    if (app.package) pid = await launchPackage(this.store,this.native,app,args,env);
    else {
      const child = spawn(app.exe, args, { cwd: app.cwd, env, windowsHide: false, detached: true, stdio: 'ignore' });
      await new Promise<void>((resolve, reject) => { child.once('spawn', resolve); child.once('error', () => reject(new Error('应用启动失败，请检查路径和权限'))); }); child.unref();
      pid = child.pid!;
    }
    mark('spawnMs');
    const identity = await this.native.identity(pid);
    if (identity && (!identity.owned || appProcesses(this.store,app,[identity]).length !== 1)) throw new Error('启动回执与目标实例身份不匹配，未登记为成功');
    if (identity) { const r = await this.store.runtime(); r.launches[id] = { identity, proxy, at: Date.now() }; await this.store.runtimeSave(r); }
    mark('receiptMs');
    await this.store.log('launch', `${id} pid=${pid} ${identity ? 'process-created' : 'exited-or-forwarded'} mode=${proxy ? 'proxy' : 'direct'}`);
    return { pid, running: !!identity, proxy, evidence: identity ? '进程已创建；目标实际流量需要另外验证' : '进程已退出或转交其他进程，未确认目标已使用代理' };
  }
  async edit(id: string, patch: Partial<Pick<App,'name'|'args'|'cwd'|'profileId'>>) {
    return this.store.lock(async () => {
      const s = await this.store.read(); const a = s.apps.find(a => a.id === id); if (!a) throw new Error('应用不存在');
      Object.assign(a, patch); if (!a.profileId) a.guard = false; this.checkArgs(a); await this.store.save(s);
      if (!s.apps.some(a => a.guard)) await this.native.call('task-remove', taskSpec(this.store));
      return a;
    });
  }
}
