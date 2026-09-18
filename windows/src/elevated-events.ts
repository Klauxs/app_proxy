import { connect, type Socket } from 'node:net';
import { createHash } from 'node:crypto';
import type { Native } from './native.ts';
import type { Store } from './store.ts';
import type { ProcessStart, ProcessWatcher } from './process-events.ts';

export interface EventsStatus { installed: boolean; current: boolean; running: boolean; task: string; pipe: string }
export function eventsKey(store: Store) { return createHash('sha256').update(store.scopeRoot.toLowerCase()).digest('hex').slice(0,12); }

export function watchGuardEvents(native: Native, key: string, onStart: (event: ProcessStart) => void, onState: (ready: boolean, reason?: string) => void): ProcessWatcher {
  let closed = false;
  let failed = false;
  let socket: Socket | undefined;
  let retry: NodeJS.Timeout | undefined;
  let timer: NodeJS.Timeout | undefined;
  function fail(reason: string) {
    if (closed || failed) return;
    failed = true; clearTimeout(retry); clearTimeout(timer); socket?.destroy(); onState(false, reason);
  }
  function open(path: string) {
    if (closed || failed) return;
    let connected = false;
    let buffer = '';
    const client = socket = connect(path);
    client.setEncoding('utf8');
    client.on('connect', () => { connected = true; });
    client.on('error', () => {
      if (closed || failed) return;
      if (connected) fail('elevated-pipe-error');
      else retry = setTimeout(() => open(path), 100);
    });
    client.on('close', () => { if (connected) fail('elevated-listener-exited'); });
    client.on('data', (chunk: string) => {
      if (closed || failed) return;
      buffer += chunk;
      if (buffer.length > 65536) { fail('invalid-listener-message'); return; }
      let end: number;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0,end); buffer = buffer.slice(end+1);
        let item: any;
        try { item = JSON.parse(line); } catch { fail('invalid-listener-message'); return; }
        if (item.type === 'ready' && item.elevated === true && item.source === 'etw') { clearTimeout(timer); onState(true, 'elevated-etw-process-start'); }
        else if (item.type === 'error' && typeof item.reason === 'string' && /^etw-[a-zA-Z0-9-]{1,100}$/.test(item.reason)) { fail(item.reason); return; }
        else if (item.type === 'start' && Number.isSafeInteger(item.pid) && item.pid > 0 && typeof item.name === 'string' && item.name.length <= 260) onStart({pid:item.pid,name:item.name,
          ...(Number.isFinite(item.eventAt) ? {eventAt:item.eventAt} : {}), ...(Number.isFinite(item.callbackAt) ? {callbackAt:item.callbackAt} : {})});
        else { fail('elevated-listener-unavailable'); return; }
      }
    });
  }
  void (async () => {
    const status = await native.call<EventsStatus>('events-status', {key});
    if (closed) return;
    if (!status.installed) { fail('listener-authorization-required'); return; }
    // Stale installed code is never silently overwritten from the ordinary process.
    if (!status.current) { fail('listener-update-requires-authorization'); return; }
    const running = await native.call<EventsStatus>('events-start', {key});
    if (closed) return;
    timer = setTimeout(() => fail('elevated-listener-timeout'), 15000);
    open(running.pipe);
  })().catch(() => fail('elevated-listener-task-error'));
  return { close() { closed = true; clearTimeout(timer); clearTimeout(retry); socket?.destroy(); } };
}
