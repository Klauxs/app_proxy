import { spawn } from 'node:child_process';
import { basename } from 'node:path';
import { Store, sleep } from './store.ts';
import { Native, sameIdentity } from './native.ts';
import { Applications, isHelper, expectedProxy, appProcesses } from './applications.ts';
import { Core } from './core.ts';
import { proxyUrl } from './proxy.ts';
import { taskSpec, cliPath } from './integration.ts';
import type { WatchProcessStarts, ProcessWatcher } from './process-events.ts';
import { eventsKey, watchGuardEvents, type EventsStatus } from './elevated-events.ts';
import type { State } from './types.ts';
export class Guard {
  private attempts = new Map<string, number[]>();
  watchStarts: WatchProcessStarts;
  readonly store: Store; readonly native: Native; readonly apps: Applications; readonly core: Core;
  constructor(store: Store, native: Native, apps: Applications, core: Core) {
    this.store = store; this.native = native; this.apps = apps; this.core = core;
    this.watchStarts = (event, state) => watchGuardEvents(this.native, eventsKey(this.store), event, state);
  }
  async eventsStatus() { return this.native.call<EventsStatus>('events-status', {key:eventsKey(this.store)}); }
  private async authorizeEvents() {
    const result = await this.native.call<EventsStatus>('events-install', {key:eventsKey(this.store)});
    if (!result.installed || !result.current) throw new Error('管理员事件监听安装未完成，未启用新的 Guard 保护');
    return result;
  }
  async configureEvents(enabled: boolean) {
    // Only this explicit foreground action may prompt for UAC; never the background retry loop.
    const result = enabled ? await this.authorizeEvents() : await this.native.call<EventsStatus>('events-remove', {key:eventsKey(this.store)});
    await this.store.log('guard-events-authorization', enabled ? 'enabled' : 'removed');
    if (enabled && (await this.store.read()).apps.some(a => a.guard)) {
      // Refresh older running Guard code and its login path, without elevating the launcher.
      await this.store.lock(async () => {
        await this.native.call('task-install', taskSpec(this.store));
        const old = await this.running();
        if (old && old.pid !== process.pid) await this.native.stop(old);
      });
      await this.start();
    }
    return result;
  }
  async enable(id: string, enabled: boolean, installTask = true) {
    const validate = (s: State) => {
      const app = s.apps.find(a => a.id === id); if (!app) throw new Error('应用不存在');
      if (enabled && (app.adapter !== 'chromium' || !app.profileId)) throw new Error('Guard 首版只保护已绑定代理、经确认支持参数的 Chromium/Electron 应用');
      if (enabled && !app.instance && s.apps.some(a => !a.instance && a.id !== id && a.exe.toLowerCase() === app.exe.toLowerCase())) throw new Error('同一 EXE 有多个普通登记，不能确定 Guard 应使用哪个绑定');
      return app;
    };
    if (enabled) {
      validate(await this.store.read());
      // Complete foreground UAC before mutating protection or taking the store lock.
      await this.authorizeEvents();
    }
    await this.store.lock(async () => {
      const s = await this.store.read(); const app = validate(s);
      app.guard = enabled;
      if (installTask && enabled) await this.native.call('task-install', taskSpec(this.store));
      await this.store.save(s);
      if (installTask && !s.apps.some(a => a.guard)) await this.native.call('task-remove', taskSpec(this.store));
    });
    if (enabled) await this.start(true);
  }
  async running() { const id = (await this.store.runtime()).guard; return id && sameIdentity(id, await this.native.identity(id.pid)) ? id : undefined; }
  async start(refresh = false) {
    await this.authorizeEvents();
    return this.store.lock(async () => {
      const old = await this.running();
      if (old && !refresh) return old;
      if (old && old.pid !== process.pid) await this.native.stop(old);
      const child = spawn(process.execPath, [cliPath, '--home', this.store.root, 'guard','run'], { detached: true, windowsHide: true, stdio: 'ignore' });
      await new Promise<void>((resolve, reject) => { child.once('spawn', resolve); child.once('error', () => reject(new Error('Guard 启动失败'))); }); child.unref();
      const id = await this.native.identity(child.pid!); if (!id) throw new Error('Guard 未能启动');
      const r = await this.store.runtime(); r.guard = id; await this.store.runtimeSave(r); return id;
    });
  }
  async tick(names?: ReadonlySet<string>): Promise<Set<string>> {
    const retry = new Set<string>();
    const s = await this.store.read();
    const revision = JSON.stringify(s);
    const guarded: typeof s.apps = [];
    // Read-only discovery must not hold the launcher lock. Recheck before acting.
    let changed = false;
    for (const app of s.apps.filter(a => a.guard && a.profileId)) {
      if (names && !names.has(basename(app.exe).toLowerCase())) continue;
      try {
        if (await this.apps.resolvePackage(app)) changed = true;
        guarded.push(app);
      } catch (e: any) { await this.store.log('guard-unavailable', `${app.id}: ${e.message}`); }
    }
    if (!guarded.length) return retry;
    const processes = await this.native.processes(guarded.map(a => a.exe));
    for (const app of guarded) {
      const matches = processes.filter(p => p.path.toLowerCase() === app.exe.toLowerCase());
      if (matches.some(p => !p.args.length) || (names && !matches.length)) retry.add(basename(app.exe).toLowerCase());
    }
    await this.store.lock(async () => {
      if (JSON.stringify(await this.store.read()) !== revision) {
        for (const app of guarded) retry.add(basename(app.exe).toLowerCase());
        return;
      }
      if (changed) await this.store.save(s);
      const runtime = await this.store.runtime();
      for (const app of guarded) {
        const p = s.profiles.find(p => p.id === app.profileId); if (!p) continue;
        const proxy = proxyUrl(p);
        // An unreadable command line must not be mistaken for an unproxied original.
        const targets = appProcesses(this.store, app, processes).filter(p => p.owned && p.args.length && !isHelper(p));
        for (const target of targets) {
          const record = runtime.launches[app.id];
          const knownOldProxy = record && sameIdentity(record.identity, target) && record.proxy && expectedProxy(target, record.proxy);
          if (expectedProxy(target, proxy) || knownOldProxy) continue;
          const history = (this.attempts.get(app.id) || []).filter(t => Date.now() - t < 60000);
          if (history.length >= 3 || (history.length && Date.now() - history.at(-1)! < 5000)) continue;
          history.push(Date.now()); this.attempts.set(app.id, history);
          const began = performance.now();
          const timings: Record<string,number> = { detectedAfterMs: Math.max(0,Date.now() - Date.parse(target.created)) };
          try {
            // Only the exact validated main instance. Never /IM or unverified tree kills.
            await this.native.stop(target, true);
            timings.stopMs = Math.round(performance.now() - began);
            const cleanupAt = performance.now();
            let checkedProcesses = await this.native.processes([app.exe]);
            let remaining = appProcesses(this.store, app, checkedProcesses);
            // Most early launches have already exited. Wait only for trailing helpers,
            // and never stop or launch over a newly started main instance.
            for (let attempt = 0; remaining.length && remaining.every(isHelper) && attempt < 3; attempt++) {
              await sleep(100);
              checkedProcesses = await this.native.processes([app.exe]);
              remaining = appProcesses(this.store, app, checkedProcesses);
            }
            if (remaining.length) throw new Error('仍有辅助进程或其他实例，请手动关闭后重试');
            timings.cleanupMs = Math.round(performance.now() - cleanupAt);
            await this.apps.launchUnlocked(s, app.id, timings, { app, processes: checkedProcesses });
            await this.store.log('guard-corrected', `${app.id} replaced=${target.pid}`);
          } catch (e: any) { await this.store.log('guard-blocked', `${app.id}: ${e.message}`); }
          finally { timings.correctionMs = Math.round(performance.now() - began); await this.store.log('guard-timing', `${app.id} pid=${target.pid} ${JSON.stringify(timings)}`); }
          break;
        }
      }
    });
    return retry;
  }
  async run(signal: AbortSignal) {
    const me = await this.native.identity(process.pid); if (!me) throw new Error('无法确认 Guard 身份');
    const own = await this.store.lock(async () => {
      const old = await this.running(); if (old && old.pid !== process.pid) return false;
      const r = await this.store.runtime(); r.guard = me; await this.store.runtimeSave(r); return true;
    });
    if (!own) return;
    let watcher: ProcessWatcher | undefined;
    let available = false;
    let stateChange: boolean | undefined;
    let failureReason: string | undefined;
    let reconnectAt = 0;
    let scanAt = 0;
    let notified = false;
    let resume: (() => void) | undefined;
    const pending = new Set<string>();
    let watchedNames = new Set<string>();
    let configuration = '';
    const retries = new Map<string, { at: number; count: number }>();
    const wake = () => { notified = true; resume?.(); };
    signal.addEventListener('abort', wake);
    try {
      while (!signal.aborted) {
        notified = false;
        const apps = (await this.store.read()).apps.filter(a => a.guard && a.profileId);
        if (!apps.length) break;
        if (JSON.stringify(apps) !== configuration) { configuration = JSON.stringify(apps); scanAt = 0; }
        watchedNames = new Set(apps.map(a => basename(a.exe).toLowerCase()));
        if (!watcher && Date.now() >= reconnectAt) {
          reconnectAt = Date.now() + 30000;
          watcher = this.watchStarts(event => {
            const name = event.name.toLowerCase();
            if (watchedNames.has(name)) { pending.add(name); wake(); }
          }, (ready, reason) => { available = ready; stateChange = ready; failureReason = reason; scanAt = 0; wake(); });
        }
        if (stateChange !== undefined) {
          const ready = stateChange;
          stateChange = undefined;
          scanAt = 0;
          if (!ready) { watcher?.close(); watcher = undefined; reconnectAt = Date.now() + 30000; }
          await this.store.log(ready ? 'guard-events-ready' : 'guard-events-fallback', ready ? (failureReason || 'process-start notifications') : `${failureReason || 'unavailable'}; using 2s scans; retry listener in 30s`);
        }
        const names = new Set(apps.map(a => basename(a.exe).toLowerCase()));
        const requested = new Set([...pending].filter(name => names.has(name)));
        pending.clear();
        for (const name of requested) retries.delete(name);
        for (const [name, retry] of retries) {
          if (!names.has(name)) retries.delete(name);
          else if (Date.now() >= retry.at) requested.add(name);
        }
        const fullScan = Date.now() >= scanAt;
        if (fullScan || requested.size) {
          try {
            const unresolved = await this.tick(fullScan ? undefined : requested);
            for (const name of fullScan ? names : requested) {
              const count = (retries.get(name)?.count || 0) + 1;
              if (unresolved.has(name) && count <= 3) retries.set(name, {at:Date.now() + 100 * count, count});
              else retries.delete(name);
            }
          } catch (e: any) {
            for (const name of fullScan ? names : requested) retries.delete(name);
            await this.store.log('guard-error', e.message);
          }
          if (fullScan) scanAt = stateChange !== undefined ? 0 : Date.now() + (available ? 30000 : 2000);
        }
        const deadline = Math.min(scanAt, watcher ? Infinity : reconnectAt, ...[...retries.values()].map(r => r.at));
        if (!notified && !signal.aborted) await new Promise<void>(resolve => {
          // Read configuration every 2s so disabling or adding protection stays responsive.
          const timer = setTimeout(done, Math.min(2000, Math.max(0, deadline - Date.now())));
          function done() { clearTimeout(timer); resume = undefined; resolve(); }
          resume = done;
        });
      }
    } finally {
      signal.removeEventListener('abort', wake);
      watcher?.close();
      await this.store.lock(async () => { const r = await this.store.runtime(); if (r.guard?.pid === process.pid) { delete r.guard; await this.store.runtimeSave(r); } });
    }
  }
}
