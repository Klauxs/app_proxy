export type NodeSpec = Record<string, any> & { name: string; protocol: string; server: string; server_port: number; selected?: boolean };
export interface Profile {
  id: string; name: string; kind: 'managed' | 'sing-box'; host: string; port: number;
  source?: { kind: 'manual' | 'subscription'; url?: string; updatedAt?: string };
  nodes: NodeSpec[];
}
export interface App {
  id: string; name: string; exe: string; cwd: string; args: string[];
  adapter: 'environment' | 'chromium'; profileId?: string; guard: boolean;
  instance?: 'codex' | 'claude';
  package?: { familyName: string; appId: string; isolatedStorage?: boolean };
}
export interface PackageRegistration { familyName: string; appId: string; exe: string; aumid: string; fullTrust: boolean; isolatedStorage?: boolean }
export interface Identity { pid: number; created: string; path: string; args: string[]; parent: number; session: number; owned: boolean }
export interface State {
  schemaVersion: 1; profiles: Profile[]; apps: App[];
  core?: { path: string; version: string; bundled?: boolean; downloaded?: boolean; selected?: boolean; sha256?: string; url?: string };
  settings: { doh?: string; testUrl: string; exitUrl: string };
  shortcuts: { appId: string; path: string }[];
}
export interface Runtime { core?: Identity; upstream?: import('./network.ts').Upstream; guard?: Identity; launches: Record<string, { identity: Identity; proxy?: string; at: number }>; }
