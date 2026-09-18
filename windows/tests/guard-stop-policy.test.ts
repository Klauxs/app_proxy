import { test } from 'node:test';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

test('native Guard stop policy immediately kills fresh direct launches and bounds older graceful exits', async () => {
  await promisify(execFile)(join(process.env.SystemRoot || 'C:\\Windows','System32/WindowsPowerShell/v1.0/powershell.exe'),
    ['-NoProfile','-NonInteractive','-File',fileURLToPath(new URL('./guard-stop-policy.ps1',import.meta.url))],
    { windowsHide:true, timeout:15000 });
});
