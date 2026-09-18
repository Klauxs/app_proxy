import * as fs from 'node:fs/promises';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import type {Native} from './native.ts';
import type {Profile,State} from './types.ts';
import {probe} from './proxy.ts';
const exec=promisify(execFile);
export interface Listener {host:string;port:number;path:string;pid:number;created:string;}
export interface Discovery {binaries:string[];listeners:Listener[];}
export async function inspectBinary(path:string):Promise<NonNullable<State['core']>> {
  const actual=await fs.realpath(path);
  const {stdout}=await exec(actual,['version'],{windowsHide:true,timeout:5000,maxBuffer:65536});
  const version=stdout.match(/^sing-box version [\w.+-]+/m)?.[0];
  if(!version) throw new Error('所选程序不是可用的 sing-box');
  return {path:actual,version};
}
export async function discovery(native:Native):Promise<Discovery> {return native.call('singbox-discover');}
export async function verifyListener(native:Native,endpoint:Pick<Profile,'host'|'port'>,testUrl:string,found?:Discovery) {
  const item=(found || await discovery(native)).listeners.find(p=>p.host===endpoint.host && p.port===endpoint.port);
  if(!item) throw new Error('该入口不是当前可识别的 sing-box 监听端口，请先启动原服务');
  const binary=await inspectBinary(item.path);
  await probe(endpoint,testUrl);
  const identity=await native.identity(item.pid);
  if(!identity || identity.created!==item.created || identity.path.toLowerCase()!==item.path.toLowerCase()) throw new Error('sing-box 服务在验证期间发生变化，请重试');
  return {...item,version:binary.version};
}
