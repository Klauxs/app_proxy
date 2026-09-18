import { parseSubscription } from './legacy/subscription.js';
import type { NodeSpec, Profile } from './types.ts';
export const agents = ['Clash.Meta','Loon/3.2.0','Quantumult X/1.5.0','Surge/5.0','Shadowrocket/2.2.0','ClashforWindows/0.20.39','ClashX Pro/1.118.1','Mozilla/5.0'];
export async function download(url: string, max = 8 * 1024 * 1024): Promise<string> {
  const u = new URL(url); if (!['http:','https:'].includes(u.protocol) || u.username || u.password) throw new Error('订阅需要 HTTP/HTTPS URL（不含用户密码）');
  for (const agent of agents) {
    try {
      const r = await fetch(u, { headers: { 'user-agent': agent, accept: '*/*' }, signal: AbortSignal.timeout(15000) });
      if (!r.ok || !r.body) { await r.body?.cancel(); continue; }
      let size = 0; const chunks = [];
      for await (const part of r.body) { size += part.length; if (size > max) throw new Error('响应过大'); chunks.push(part); }
      return Buffer.concat(chunks).toString('utf8');
    } catch { /* Try the next original client user agent. Do not expose URL. */ }
  }
  throw new Error('订阅下载失败：检查地址、网络及服务商；已尝试原版客户端标识');
}
export function parse(text: string): { nodes: NodeSpec[]; unsupported: { protocol: string; reason: string; source_index?: number }[] } {
  try {
    const result = parseSubscription(text); if (!result) throw new Error('empty');
    // JSON.parse errors can quote credential-bearing input. Expose category/index only.
    result.unsupported = result.unsupported.map((item: any) => ({
      protocol: /^[\w-]{1,32}$/.test(item.protocol) ? item.protocol : 'unknown',
      reason: /^unsupported /.test(item.reason) ? '不支持此格式或协议' : /^missing /.test(item.reason) ? '缺少必填字段' : '节点格式或字段无效',
      source_index: item.source_index
    }));
    return result;
  } catch { throw new Error('订阅解析失败或内容为空'); }
}
export function align(profile: Profile, nodes: NodeSpec[]) {
  const old = new Map(profile.nodes?.map(n => [n.name, n])); const seen = new Set<string>();
  for (const n of nodes) { if (seen.has(n.name)) throw new Error('订阅含重名节点，请先在来源修正名称'); seen.add(n.name); }
  const next = nodes.map(n => ({ ...n, selected: !!old.get(n.name)?.selected }));
  const removed = profile.nodes?.filter(n => n.selected && !seen.has(n.name)).map(n => n.name) || [];
  if (!next.some(n => n.selected)) throw new Error('更新将清空已选出口，已保留旧配置；请重新导入并选择节点');
  return { nodes: next, removed, added: next.filter(n => !old.has(n.name)).length };
}
