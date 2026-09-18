import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import type { Identity } from './types.ts';
export class Native {
  private child?: ChildProcessWithoutNullStreams;
  private seq = 0;
  private pending = new Map<number, { resolve: (value: any) => void; reject: (error: Error) => void; timer: NodeJS.Timeout }>();
  async call<T = any>(op: string, data: any = {}): Promise<T> {
    if (process.platform !== 'win32') throw new Error('仅支持 Windows');
    if (!this.child) {
      const ps = join(process.env.SystemRoot || 'C:\\Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe');
      const env = { ...process.env };
      for (const key of Object.keys(env)) if (key.toLowerCase() === 'psmodulepath') delete env[key];
      env.PSModulePath = join(process.env.SystemRoot || 'C:\\Windows', 'System32/WindowsPowerShell/v1.0/Modules');
      this.child = spawn(ps, ['-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', fileURLToPath(new URL('../native/bridge.ps1', import.meta.url))], { windowsHide: true, stdio: 'pipe', env });
      const fail = () => { for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(new Error('Windows 辅助进程已退出')); } this.pending.clear(); this.child = undefined; };
      this.child.on('error', fail); this.child.on('exit', fail);
      this.child.stderr.resume();
      createInterface({ input: this.child.stdout }).on('line', line => {
        try {
          const result = JSON.parse(line); const p = this.pending.get(result.id); if (!p) return;
          clearTimeout(p.timer); this.pending.delete(result.id);
          result.ok ? p.resolve(result.result) : p.reject(new Error(String(result.error)));
        } catch { /* Ignore non-protocol diagnostics; never print secrets. */ }
      });
    }
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      const timeout = ['events-install','events-remove'].includes(op) ? 150000 : 30000;
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error('Windows 操作超时')); }, timeout);
      this.pending.set(id, { resolve, reject, timer });
      this.child!.stdin.write(JSON.stringify({ id, op, ...data }) + '\n');
    });
  }
  async processes(paths: string[] = []): Promise<Identity[]> { const r = await this.call('processes', { paths }); return Array.isArray(r) ? r : r ? [r] : []; }
  async identity(pid: number): Promise<Identity | undefined> { return (await this.call<Identity | null>('identity', { target: pid })) || undefined; }
  async stop(identity: Identity) { return this.call('stop', { identity }); }
  close() { this.child?.stdin.end(); }
}
export function sameIdentity(a: Identity | undefined, b: Identity | undefined) {
  return !!a && !!b && a.pid === b.pid && a.created === b.created && a.path.toLowerCase() === b.path.toLowerCase() && b.owned;
}
