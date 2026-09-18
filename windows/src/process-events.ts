export interface ProcessStart { pid: number; name: string; eventAt?: number; callbackAt?: number }
export interface ProcessWatcher { close(): void }
export type WatchProcessStarts = (onStart: (event: ProcessStart) => void, onState: (ready: boolean, reason?: string) => void) => ProcessWatcher;
