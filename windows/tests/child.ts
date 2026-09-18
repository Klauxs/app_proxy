import { writeFileSync } from 'node:fs';
import { proxyRequest } from '../src/proxy.ts';
const output = process.argv[2];
const requestArg=process.argv.find(a=>a.startsWith('--request-url='));
let response;
if(requestArg && process.env.HTTP_PROXY){const proxy=new URL(process.env.HTTP_PROXY);response=await proxyRequest({host:proxy.hostname,port:Number(proxy.port)},requestArg.slice('--request-url='.length));}
writeFileSync(output, JSON.stringify({ args:process.argv.slice(3), cwd:process.cwd(), proxy:process.env.HTTP_PROXY, https:process.env.HTTPS_PROXY, all:process.env.ALL_PROXY, no:process.env.NO_PROXY, codexHome:process.env.CODEX_HOME, userData:process.env.CODEX_ELECTRON_USER_DATA_PATH, claudeHome:process.env.CLAUDE_CONFIG_DIR, pid:process.pid, response }));
setInterval(()=>{},1000);
