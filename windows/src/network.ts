import type { Native } from './native.ts';

export interface NetworkAdapter {
  name: string; index: number; up: boolean; hardware: boolean; virtual: boolean;
  wifi: boolean; addresses: string[]; metric: number | null;
}
export interface Upstream {
  mode: 'physical' | 'auto'; interface?: string; reason?: string; verifiedAt?: string;
}

// Hardware flags are authoritative; names are an additional defense against tunnel drivers.
export function physicalCandidates(adapters: NetworkAdapter[]) {
  return adapters.filter(a => a.up && a.hardware && !a.virtual && a.name &&
    !/(?:\b(?:tun|tap|vpn|wintun|wireguard|loopback|virtual|bridge|bluetooth)\b|vEthernet|虚拟|蓝牙)/i.test(a.name) &&
    a.addresses.some(ip => !/^(?:127\.|169\.254\.|0\.)/.test(ip)) && a.metric !== null)
    .sort((a, b) => Number(b.wifi) - Number(a.wifi) || a.metric! - b.metric! || a.index - b.index);
}

export async function upstreamChoices(native: Native): Promise<Upstream[]> {
  try {
    const adapters = await native.call<NetworkAdapter[]>('network-adapters');
    const candidates = physicalCandidates(adapters);
    return [...candidates.map(a => ({ mode: 'physical' as const, interface: a.name })),
      { mode: 'auto', reason: candidates.length ? '物理网卡联网验证失败，已回退到 sing-box 自动路由，可能经过虚拟网卡' : '未找到可用物理网卡，使用 sing-box 自动路由，可能经过虚拟网卡' }];
  } catch {
    return [{ mode: 'auto', reason: '无法枚举物理网卡，使用 sing-box 自动路由，可能经过虚拟网卡' }];
  }
}
