import { Store, uid, port } from './store.ts';
import { Native } from './native.ts';
import { Core } from './core.ts';
import { Applications, defaultGuard } from './applications.ts';
import { Guard } from './guard.ts';
import { shortcut, launchSpec, taskSpec } from './integration.ts';
import { proxyUrl, probe, proxyRequest } from './proxy.ts';
import { download, parse, align } from './subscription.ts';
import type { Profile, NodeSpec, State } from './types.ts';
import { discovery, verifyListener } from './singbox.ts';
export class Service {
  store: Store; native = new Native(); core: Core; apps: Applications; guard: Guard;
  constructor(home?: string) { this.store = new Store(home); this.core = new Core(this.store, this.native); this.apps = new Applications(this.store, this.native, this.core); this.guard = new Guard(this.store, this.native, this.apps, this.core); }
  async init() { await this.store.init(this.native); return this; }
  close() { this.native.close(); }
  async addApp(data: Parameters<Applications['add']>[0] & {guard?: boolean}) {
    if (data.guard !== undefined && typeof data.guard !== 'boolean') throw new Error('guard 必须为 true 或 false');
    const app = await this.apps.add(data);
    if (data.guard ?? defaultGuard(app)) {
      try { await this.guard.enable(app.id,true); }
      catch (error: any) { throw new Error(`应用已添加（${app.id}），但 Guard 设置或启动未完成：${error.message}。可在“6 Guard”中查看状态并重新启用`); }
      app.guard = true;
    }
    return app;
  }
  async discoverSingBox() {
    const state = await this.store.read(); const active = await this.core.running();
    const found = await discovery(this.native);
    const available = []; const unavailable = [];
    for (const item of found.listeners.filter(p => p.pid !== active?.pid && !state.profiles.some(profile => profile.host === p.host && profile.port === p.port))) {
      try { available.push(await verifyListener(this.native,item,state.settings.testUrl,found)); }
      catch (e: any) { unavailable.push({host:item.host,port:item.port,reason:e.message}); }
    }
    return {binaries:found.binaries,available,unavailable};
  }
  async useSingBox(name: string, listenPort: number, host = '127.0.0.1') {
    if (!['127.0.0.1','::1'].includes(host)) throw new Error('只复用本机 sing-box 入口');
    return this.store.lock(async () => {
      const state = await this.store.read();
      const p: Profile = {id:uid(),name,kind:'sing-box',host,port:port(listenPort),nodes:[]};
      const found = await verifyListener(this.native,p,state.settings.testUrl);
      if (found.pid === (await this.core.running())?.pid) throw new Error('这是本工具启动的入口，请直接使用已有代理登记');
      state.profiles.push(p); await this.store.save(state); return p;
    });
  }
  async addManaged(name: string, listenPort: number, nodes: NodeSpec[], source: Profile['source'] = { kind: 'manual' }) {
    const p: Profile = { id: uid(), name, kind: 'managed', host: '127.0.0.1', port: port(listenPort), nodes, source };
    await this.store.lock(async () => { const s = await this.store.read(); s.profiles.push(p); await this.core.applyUnlocked(s); }); return p;
  }
  async refresh(id: string) {
    const initial = await this.store.read(); const old = initial.profiles.find(p => p.id === id);
    if (old?.kind !== 'managed' || !old.source?.url) throw new Error('该代理不是本工具管理的订阅来源');
    const fetched = parse(await download(old.source.url));
    return this.store.lock(async () => {
      const s = await this.store.read(); const p = s.profiles.find(p => p.id === id);
      if (!p || p.source?.url !== old.source?.url) throw new Error('订阅来源已改变，请重新刷新');
      const result = align(p, fetched.nodes); p.nodes = result.nodes; p.source!.updatedAt = new Date().toISOString();
      await this.core.applyUnlocked(s); return { added: result.added, removed: result.removed, unsupported: fetched.unsupported.length };
    });
  }
  async editProfile(id: string, patch: Partial<Pick<Profile,'name'|'port'|'nodes'>>) {
    return this.store.lock(async () => {
      const s = await this.store.read(); const p = s.profiles.find(p => p.id === id); if (!p) throw new Error('代理不存在');
      if (p.kind === 'sing-box') {
        if (patch.port !== undefined || patch.nodes !== undefined) throw new Error('已有 sing-box 的端口和节点由原服务管理；这里只能改名或解除登记');
        Object.assign(p,patch); await this.store.save(s); return p;
      }
      Object.assign(p, patch); await this.core.applyUnlocked(s); return p;
    });
  }
  async removeProfile(id: string) {
    await this.store.lock(async () => {
      const s = await this.store.read(); const p = s.profiles.find(p => p.id === id); if (!p) throw new Error('代理不存在');
      if (s.apps.some(a => a.profileId === id)) throw new Error('仍有应用引用此代理，请先解除绑定');
      s.profiles = s.profiles.filter(p => p.id !== id);
      if (p.kind === 'managed') await this.core.applyUnlocked(s); else await this.store.save(s);
    });
  }
  async removeApp(id: string) {
    await this.guard.enable(id, false); await shortcut(this.store, this.native, id, true);
    await this.store.lock(async () => { const s = await this.store.read(); s.apps = s.apps.filter(a => a.id !== id); await this.store.save(s); });
  }
  async settings(patch: Partial<State['settings']>) {
    await this.store.lock(async () => { const s = await this.store.read(); Object.assign(s.settings, patch); if (s.profiles.some(p => p.kind === 'managed')) await this.core.applyUnlocked(s); else await this.store.save(s); });
  }
  async status() {
    const s = await this.store.read(); const runtime = await this.store.runtime();
    const activeCore = await this.core.running();
    return {
      dataDirectory: this.store.root, core: { ...s.core, pid: activeCore?.pid, runningPath: activeCore?.path, upstream: activeCore ? runtime.upstream : undefined },
      guardPid: (await this.guard.running())?.pid,
      profiles: s.profiles.map(p => ({ id: p.id, name: p.name, kind: p.kind, address: proxyUrl(p), selected: p.nodes.filter(n => n.selected).map(n => n.name) })),
      apps: s.apps.map(a => ({ ...a, args: `[${a.args.length} arguments; hidden]`, lastPid: runtime.launches[a.id]?.identity.pid })),
      evidence: '配置绑定和启动记录不是流量证据；doctor 的出口请求来自工具，不代表目标应用全部使用代理'
    };
  }
  async doctor(id?: string) {
    const s = await this.store.read(); const profiles = s.profiles.filter(p => !id || p.id === id); if (id && !profiles.length) throw new Error('代理不存在');
    const results = [];
    for (const p of profiles) {
      try {
        if (p.kind === 'sing-box') await verifyListener(this.native,p,s.settings.testUrl);
        const health = await probe(p, s.settings.testUrl);
        let exit = '未探测'; try { const r = await proxyRequest(p, s.settings.exitUrl); exit = r.status === 200 && /^[\da-f.:\s]+$/i.test(r.body.trim()) ? r.body.trim() : `出口端点返回 HTTP ${r.status} 或非 IP 内容`; } catch { exit = '出口探测失败，可能是目标站点不可达'; }
        results.push({ id: p.id, name: p.name, healthy: true, status: health.status, exit });
      } catch (e: any) { results.push({ id: p.id, name: p.name, healthy: false, reason: e.message }); }
    }
    return { probes: results, ...(await this.status()) };
  }
  async uninstall(mode: 'all'|'core') {
    // Revocation needs UAC. Complete it before changing any user configuration.
    if (mode === 'all' && (await this.guard.eventsStatus()).installed) await this.guard.configureEvents(false);
    await this.store.lock(async () => {
      const s = await this.store.read();
      if (mode === 'core') {
        for (const a of s.apps) if (s.profiles.some(p => p.id === a.profileId && p.kind === 'managed')) a.guard = false;
        await this.store.save(s);
        if (!s.apps.some(a => a.guard)) {
          const id = await this.guard.running(); if (id && id.pid !== process.pid) await this.native.stop(id);
          await this.native.call('task-remove', taskSpec(this.store));
        }
      }
      if (mode === 'all') {
        // Disable protection first so no new application can be restarted during cleanup.
        for (const a of s.apps) a.guard = false; await this.store.save(s);
        const guard = await this.guard.running(); if (guard && guard.pid !== process.pid) await this.native.stop(guard);
        await this.native.call('task-remove', taskSpec(this.store));
        for (const item of s.shortcuts) await this.native.call('shortcut-remove', { path: item.path, ...launchSpec(this.store, ['--notify','launch',item.appId]) });
        s.shortcuts = []; s.apps = [];
      }
      await this.core.stopUnlocked();
      // Retain manifests, previous configuration and app data for recovery. No recursive data deletion.
      if (mode === 'all') s.profiles = [];
      // Installed sing-box programs are reusable dependencies, not uninstall targets.
      delete s.core; await this.store.save(s);
      await this.store.log('uninstall', `mode=${mode}; application data and backups retained`);
    });
  }
}
