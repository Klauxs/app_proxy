import net from 'node:net';
import http from 'node:http';
import https from 'node:https';
import tls from 'node:tls';
import type { Profile } from './types.ts';
export function proxyUrl(p: Pick<Profile, 'host'|'port'>) { return `http://${p.host.includes(':') ? '[' + p.host + ']' : p.host}:${p.port}`; }
export async function tcp(host: string, port: number, timeout = 1500): Promise<boolean> {
  return new Promise(resolve => { const s = net.connect({ host, port }); let done = false; const finish = (ok: boolean) => { if (!done) { done = true; s.destroy(); resolve(ok); } }; s.setTimeout(timeout, () => finish(false)); s.on('connect', () => finish(true)); s.on('error', () => finish(false)); });
}
export async function proxyRequest(p: Pick<Profile,'host'|'port'>, url: string, options: { timeout?: number; ca?: string } = {}): Promise<{ status: number; body: string }> {
  const target = new URL(url); if (!['http:', 'https:'].includes(target.protocol) || target.username || target.password) throw new Error('无效探测地址');
  const timeout = options.timeout ?? 10000;
  let socket: net.Socket | undefined; let agent: https.Agent | undefined;
  try {
    if (target.protocol === 'https:') {
      socket = await new Promise<net.Socket>((resolve, reject) => {
        const r = http.request({ host: p.host, port: p.port, method: 'CONNECT', path: `${target.hostname}:${target.port || 443}`, agent: false });
        const timer = setTimeout(() => r.destroy(new Error('代理 CONNECT 超时')), timeout);
        r.on('connect', (res, s, head) => { clearTimeout(timer); if (res.statusCode !== 200) { s.destroy(); reject(new Error(`代理 CONNECT 返回 ${res.statusCode}`)); } else { if (head.length) s.unshift(head); resolve(s); } });
        r.on('response', res => { clearTimeout(timer); res.resume(); r.destroy(); reject(new Error('端口未接受代理 CONNECT')); });
        r.on('error', () => { clearTimeout(timer); reject(new Error('代理 CONNECT 失败')); }); r.end();
      });
      agent = new https.Agent();
      agent.createConnection = () => tls.connect({ socket, servername: net.isIP(target.hostname) ? undefined : target.hostname, ca: options.ca, rejectUnauthorized: true });
    }
    return await new Promise((resolve, reject) => {
      const request = target.protocol === 'https:' ? https.request : http.request;
      const config = target.protocol === 'https:' ? { hostname: target.hostname, port: target.port || 443, path: target.pathname + target.search, agent } : { host: p.host, port: p.port, path: target.href, agent: false as const };
      const r = request({ ...config, method: 'GET', headers: { Host: target.host, Connection: 'close' } }, res => {
        let size = 0; const chunks: Buffer[] = [];
        res.on('data', (chunk: Buffer) => { size += chunk.length; if (size > 1024 * 1024) r.destroy(new Error('响应过大')); else chunks.push(chunk); });
        res.on('end', () => resolve({ status: res.statusCode || 0, body: Buffer.concat(chunks).toString('utf8') }));
        res.on('error', () => reject(new Error('代理响应中断')));
      });
      const timer = setTimeout(() => r.destroy(new Error('代理请求超时')), timeout);
      r.on('close', () => clearTimeout(timer)); r.on('error', () => reject(new Error('代理 HTTP/HTTPS 请求失败（连接、TLS 或超时）'))); r.end();
    });
  } finally { agent?.destroy(); socket?.destroy(); }
}
export async function probe(p: Pick<Profile,'host'|'port'>, url: string) {
  if (!(await tcp(p.host, p.port))) throw new Error(`代理入口 ${p.host}:${p.port} 未监听，请先启动代理或检查端口`);
  const result = await proxyRequest(p, url);
  if (result.status < 200 || result.status >= 400) throw new Error(`代理探测返回 HTTP ${result.status}；请检查上游和探测地址`);
  return result;
}
