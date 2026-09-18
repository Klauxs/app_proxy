import * as fs from 'node:fs/promises';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { dirname } from 'node:path';
import { Store, atomic, exists, sleep } from './store.ts';
import { Native, sameIdentity } from './native.ts';
import { buildManifestConfig } from './legacy/config.js';
import { tcp, probe } from './proxy.ts';
import type { State } from './types.ts';
import { builtinPath, verifyBuiltin, verifyDirectory, release } from './builtin.ts';
import { discovery, inspectBinary } from './singbox.ts';
import { installCore } from './install-core.ts';
import { upstreamChoices, type Upstream } from './network.ts';
const exec = promisify(execFile);
export function generate(state: State, upstream: Upstream = { mode: 'auto' }): any {
  const profiles = state.profiles.filter(p => p.kind === 'managed').map(p => ({ id: p.id, listen_port: p.port, nodes: p.nodes }));
  if (!profiles.length) return null;
  const config: any = buildManifestConfig({ profiles, settings: { doh_server: state.settings.doh } });
  // Windows interface aliases may contain spaces or Chinese characters. Pass as JSON, not shell text.
  delete config.route.auto_detect_interface;
  delete config.route.default_interface;
  if (upstream.mode === 'physical' && upstream.interface) config.route.default_interface = upstream.interface;
  else config.route.auto_detect_interface = true;
  config.log = { disabled: true }; // Application URLs and credentials never go into raw core logs.
  return config;
}
export class Core {
  readonly store: Store; readonly native: Native;
  private resolved?: NonNullable<State['core']>;
  constructor(store: Store, native: Native) { this.store = store; this.native = native; }
  async binary(state: State): Promise<NonNullable<State['core']>> {
    if (this.resolved) return this.resolved;
    if (state.core?.selected) return this.resolved = {...await inspectBinary(state.core.path),selected:true};
    const found = await discovery(this.native);
    for (const path of [...found.binaries,...(state.core && !state.core.bundled && !state.core.downloaded ? [state.core.path] : [])]) {
      if (path.toLowerCase() === builtinPath.toLowerCase() || path.toLowerCase().startsWith(this.store.path('bin').toLowerCase()+'\\')) continue;
      try { return this.resolved = await inspectBinary(path); } catch { /* Try the next installed executable. */ }
    }
    const destination = this.store.path('bin',`sing-box-${release.version}`);
    try { return this.resolved = {...await verifyDirectory(destination),downloaded:true}; } catch { /* Not installed by this tool. */ }
    try { return this.resolved = await verifyBuiltin(); } catch { /* Portable core absent: install a standalone copy. */ }
    const cache = this.store.path('bin','download');
    await this.store.assertSafe(destination); await this.store.assertSafe(cache);
    return this.resolved = await installCore(destination,cache);
  }
  async prepare(path?: string) {
    return this.store.lock(async () => {
      const state = await this.store.read();
      this.resolved = undefined;
      const info = path ? {...await inspectBinary(path),selected:true} : await this.binary(state);
      state.core = info; await this.store.save(state); return info;
    });
  }
  async running() {
    const identity = (await this.store.runtime()).core;
    return identity && sameIdentity(identity, await this.native.identity(identity.pid)) ? identity : undefined;
  }
  async check(state: State, path = this.store.path('config/candidate.json'), upstream?: Upstream) {
    const config = generate(state, upstream || (await upstreamChoices(this.native))[0]); if (!config) throw new Error('尚无托管代理配置');
    state.core = await this.binary(state);
    await this.store.assertSafe(path); await atomic(path, config);
    try { await exec(state.core.path, ['check', '-c', path], { timeout: 15000, windowsHide: true, maxBuffer: 1024 * 1024 }); }
    catch { throw new Error('sing-box check 未通过：节点字段或内核版本不兼容，旧配置保留（原始错误可能含凭据，未写入日志）'); }
    return config;
  }
  // All methods below require the shared store lock, unless marked public.
  async stopUnlocked() {
    const id = await this.running(); if (id) await this.native.stop(id);
    const runtime = await this.store.runtime(); delete runtime.core; delete runtime.upstream; await this.store.runtimeSave(runtime);
  }
  async spawnUnlocked(state: State) {
    state.core = await this.binary(state);
    const profiles = state.profiles.filter(p => p.kind === 'managed');
    if (!profiles.length) throw new Error('尚无托管代理');
    for (const p of profiles) if (await tcp(p.host, p.port)) throw new Error(`端口 ${p.port} 已占用，不会停止外部进程`);
    const child = spawn(state.core.path, ['run', '-c', this.store.path('config/sing-box.json')], { cwd: dirname(state.core.path), detached: true, windowsHide: true, stdio: 'ignore' });
    await new Promise<void>((resolve, reject) => { child.once('spawn', resolve); child.once('error', () => reject(new Error('内核创建失败'))); }); child.unref();
    let id;
    for (let i = 0; i < 15 && !id; i++) { id = await this.native.identity(child.pid!); if (!id) await sleep(100); }
    if (!id?.owned) throw new Error('内核启动后已退出或无法确认归属');
    const runtime = await this.store.runtime(); runtime.core = id; await this.store.runtimeSave(runtime);
    try {
      let ready = false;
      for (let i = 0; i < 40; i++) {
        if (!(await this.running())) throw new Error('内核启动后退出');
        if ((await Promise.all(profiles.map(p => tcp(p.host, p.port, 300)))).every(Boolean)) { ready = true; break; }
        await sleep(100);
      }
      if (!ready) throw new Error('内核代理入口未就绪');
      return id;
    } catch (e) { await this.stopUnlocked(); throw e; }
  }
  async startUnlocked(state: State, profileId?: string) {
    const running = await this.running();
    if (running) return running;
    const id = await this.startDetectedUnlocked(state, undefined, profileId ? [profileId] : undefined); await this.store.save(state); return id;
  }
  async startDetectedUnlocked(state: State, choices?: Upstream[], requiredProfileIds?: string[]) {
    // Do not treat a conflicting listener as an interface failure and keep retrying.
    const managed = state.profiles.filter(p => p.kind === 'managed');
    if (!managed.length) throw new Error('没有需要启动的自有配置；已有 sing-box 服务由原启动器管理');
    for (const p of managed) if (await tcp(p.host, p.port)) throw new Error(`端口 ${p.port} 已占用，不会停止外部进程`);
    let lastError: unknown;
    for (const upstream of choices || await upstreamChoices(this.native)) {
      const config = await this.check(state, undefined, upstream);
      await atomic(this.store.path('config/sing-box.json'), config);
      try {
        const id = await this.spawnUnlocked(state);
        const profiles = requiredProfileIds ? managed.filter(p => requiredProfileIds.includes(p.id)) : managed;
        if (!profiles.length) throw new Error('没有可验证的代理配置');
        const probes = profiles.map(p => probe(p, state.settings.testUrl));
        // A cold app launch needs its own exit; a general start only needs one usable exit.
        if (requiredProfileIds) await Promise.all(probes);
        else await Promise.any(probes).catch((error: AggregateError) => { throw error.errors[0] || error; });
        const runtime = await this.store.runtime(); runtime.upstream = { ...upstream, verifiedAt: new Date().toISOString() }; await this.store.runtimeSave(runtime);
        await this.store.log('upstream', upstream.interface || upstream.reason || 'auto');
        return id;
      } catch (e) {
        lastError = e; await this.stopUnlocked();
        await this.store.log('upstream-failed', upstream.interface || 'auto');
      }
    }
    throw new Error('自动网卡选择及代理联网验证失败，未启动内核：' + (lastError instanceof Error ? lastError.message : '无可用出口'));
  }
  async start() { return this.store.lock(async () => this.startUnlocked(await this.store.read())); }
  async stop() { return this.store.lock(() => this.stopUnlocked()); }
  async restart() { return this.store.lock(async () => { const s = await this.store.read(); if (await this.running()) { await this.applyUnlocked(s); return (await this.running())!; } return this.startUnlocked(s); }); }
  async applyUnlocked(next: State) {
    const old = await this.store.read(); const oldProcess = await this.running(); const wasRunning = !!oldProcess;
    const changedProfiles = next.profiles.filter(p => p.kind === 'managed').filter(p => {
      const previous = old.profiles.find(item => item.id === p.id);
      return !previous || p.host !== previous.host || p.port !== previous.port || JSON.stringify(p.nodes) !== JSON.stringify(previous.nodes);
    }).map(p => p.id);
    const oldUpstream = (await this.store.runtime()).upstream;
    const oldConfig = await exists(this.store.path('config/sing-box.json')) ? JSON.parse(await fs.readFile(this.store.path('config/sing-box.json'), 'utf8')) : null;
    const choices = await upstreamChoices(this.native);
    const config = next.profiles.some(p => p.kind === 'managed') ? await this.check(next, undefined, choices[0]) : null;
    await atomic(this.store.path('config/backup.json'), { manifest: old, config: oldConfig });
    try {
      if (wasRunning) await this.stopUnlocked();
      if (config) await atomic(this.store.path('config/sing-box.json'), config);
      if (wasRunning && config) {
        await this.startDetectedUnlocked(next, choices, changedProfiles.length ? changedProfiles : undefined);
      }
      await this.store.save(next);
    } catch (e) {
      await this.stopUnlocked(); if (oldConfig) await atomic(this.store.path('config/sing-box.json'), oldConfig);
      await this.store.save(old);
      try {
        if (wasRunning && oldConfig) {
          this.resolved = old.core?.path.toLowerCase() === oldProcess!.path.toLowerCase() ? {...old.core,...await inspectBinary(oldProcess!.path)} : await inspectBinary(oldProcess!.path);
          await this.spawnUnlocked(old);
          const runtime = await this.store.runtime(); runtime.upstream = oldUpstream; await this.store.runtimeSave(runtime);
        }
        await this.store.save(old);
      }
      catch { throw new Error('更新失败，旧配置已恢复，但旧内核也无法启动，请运行 doctor'); }
      throw e;
    }
  }
}
