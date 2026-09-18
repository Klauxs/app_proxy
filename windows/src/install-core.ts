// Install a standalone official core only when no usable executable is available.
import * as fs from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { release, verifyDirectory } from './builtin.ts';

export async function installCore(destination: string, cache: string) {
  try { return {...await verifyDirectory(destination),downloaded:true as const}; } catch { /* Stage from the pinned official archive. */ }
  await fs.mkdir(cache, { recursive: true });
  const archive = join(cache, 'sing-box.zip');
  let bytes: Buffer;
  try { bytes = await fs.readFile(archive); }
  catch {
    const response = await fetch(release.url, { signal: AbortSignal.timeout(120000) });
    if (!response.ok || !response.body) throw new Error('官方内核下载失败');
    const parts: Uint8Array[] = []; let size = 0;
    for await (const part of response.body) { size += part.length; if (size > 100 * 1024 * 1024) throw new Error('内核制品过大'); parts.push(part); }
    bytes = Buffer.concat(parts);
  }
  if (createHash('sha256').update(bytes).digest('hex') !== release.sha256) throw new Error('官方内核压缩包 SHA256 不符');
  await fs.writeFile(archive, bytes);
  await fs.mkdir(destination, { recursive: true });
  const ps = join(process.env.SystemRoot || 'C:\\Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe');
  await promisify(execFile)(ps, ['-NoProfile', '-NonInteractive', '-File', fileURLToPath(new URL('../native/extract-core.ps1', import.meta.url)), archive, destination], { windowsHide: true, timeout: 30000 });
  await fs.writeFile(join(destination, 'SOURCE.txt'), `Unmodified official sing-box ${release.version} Windows amd64 release\nBinary archive: ${release.url}\nSHA256: ${release.sha256}\nCorresponding source: https://github.com/SagerNet/sing-box/tree/v${release.version}\nSource archive: https://github.com/SagerNet/sing-box/archive/refs/tags/v${release.version}.tar.gz\nLicense: GPL-3.0-or-later; see LICENSE.\n`);
  return {...await verifyDirectory(destination),downloaded:true as const};
}
