import {resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {installCore} from '../src/install-core.ts';
import {builtinDirectory,verifyBuiltin} from '../src/builtin.ts';
export async function setupCore() {
  await installCore(builtinDirectory,fileURLToPath(new URL('../.tools/',import.meta.url)));
  return verifyBuiltin();
}
if(process.argv[1] && resolve(process.argv[1])===fileURLToPath(import.meta.url)) console.log(await setupCore());
