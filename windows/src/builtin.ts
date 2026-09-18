import * as fs from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { join } from 'node:path';

export const release = {
  version: '1.14.1', arch: 'x64',
  url: 'https://github.com/SagerNet/sing-box/releases/download/v1.14.1/sing-box-1.14.1-windows-amd64.zip',
  sha256: '5197f16d492d93202dc623622149a6ed040f8eca263128f91d603f2b901baa89',
  files: {
    'sing-box.exe': 'b838de45bd0b2e6ddbed1977e4745622f7dffab3b293807ff4c6b1b640fed909',
    'libcronet.dll': '3217c6260fbca5f16072e0b79735742f40109a63bb0ff88fd6b96dd6b54a2928'
  }
};
export const builtinDirectory = fileURLToPath(new URL('../runtime/sing-box/', import.meta.url));
export const builtinPath = fileURLToPath(new URL('../runtime/sing-box/sing-box.exe', import.meta.url));
export async function verifyDirectory(directory: string) {
  if (process.arch !== release.arch) throw new Error('此便携包只支持 Windows x64');
  for (const [name, hash] of Object.entries(release.files)) {
    let bytes: Buffer;
    try { bytes = await fs.readFile(join(directory,name)); }
    catch { throw new Error('sing-box 安装文件缺失'); }
    if (createHash('sha256').update(bytes).digest('hex') !== hash) throw new Error('sing-box 安装文件完整性校验失败');
  }
  return { path: join(directory,'sing-box.exe'), version: `sing-box version ${release.version}`, sha256: release.files['sing-box.exe'] };
}
export async function verifyBuiltin() { return {...await verifyDirectory(builtinDirectory),bundled:true as const}; }
