import { createInterface } from 'node:readline/promises';
import { readFile } from 'node:fs/promises';
import { Service } from './service.ts';
import { parse, download } from './subscription.ts';
import { port, redact } from './store.ts';
import { shortcut } from './integration.ts';
import type { NodeSpec, Profile } from './types.ts';
import { tcp } from './proxy.ts';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const help = `Windows App Proxy 0.2
不带命令打开中文菜单。所有命令都可加 --home <数据目录>。

  status                         配置摘要（隐藏凭据）
  doctor [profileId]             真实代理及出口探测
  core discover                  查找程序及可用的已有 sing-box 服务
  core verify|install            查找可用程序，缺失时安装
  core use <sing-box.exe>         指定已有程序，下次重启自有实例时使用
  core start|stop|restart|check
  proxy use-singbox <name> <port> [127.0.0.1|::1]
  proxy add-manual <name> <listenPort> <node.json>
  proxy add-subscription <name> <listenPort> <source.json>
  proxy refresh <id>             更新订阅，保留选择
  proxy select <id> <1,2,3>       修改所选节点
  proxy edit <id> <patch.json>    修改 name/port
  proxy remove <id>              删除未被引用的代理
  app add <app.json>             登记应用；Codex/Claude 桌面应用绑定代理后默认启用 Guard，可用 guard:false 关闭
  app add codex|claude <profileId|direct> [--clone]  自动查找桌面版，默认使用原版
  app bind <id> <profileId|direct>
  app edit <id> <patch.json>      修改 name/cwd/args
  app remove <id>                删除登记与快捷方式，保留应用数据
  launch <appId>
  shortcut create|remove <appId>
  guard enable|disable <appId>   启用时自动授权管理员监听及登录任务
  guard start|status|run         run 用于后台任务
  guard events enable|disable|status  修复/撤销监听授权或查看状态（可能弹 UAC）
  settings <settings.json>       doh/testUrl/exitUrl
  uninstall all|core             默认保留数据和备份，不删除原应用

source.json: {"url":"订阅地址","selected":[1,2]}
node.json: {"protocol":"http|socks5","server":"...","server_port":1234,"username":"...","password":"..."}
app.json: {"name":"应用","exe":"C:\\\\...\\\\App.exe","adapter":"chromium|environment","args":[],"profileId":"..."}
命令中的 <...> 是占位符。含凭据内容使用文件或交互菜单，不放在命令行。`;
const out = (v: unknown) => console.log(typeof v === 'string' ? v : JSON.stringify(v, null, 2));
const json = async (path: string) => JSON.parse(await readFile(path, 'utf8'));
const need = (v: string | undefined, what: string) => { if (!v) throw new Error('缺少参数：' + what); return v; };
function manual(value: any): NodeSpec {
  if (!['http','socks5'].includes(value.protocol)) throw new Error('手动节点首版支持 http / socks5');
  return { name: String(value.name || value.protocol), protocol: value.protocol, server: String(value.server || ''), server_port: port(value.server_port), username: value.username ? String(value.username) : undefined, password: value.password ? String(value.password) : undefined, tls: value.tls === true, selected: true };
}
function select(nodes: NodeSpec[], input: string) {
  const indexes = input.split(',').map(x => Number(x.trim()) - 1);
  if (!indexes.length || indexes.some(n => !Number.isInteger(n) || n < 0 || n >= nodes.length)) throw new Error('节点编号无效');
  return nodes.map((n,i) => ({ ...n, selected: indexes.includes(i) }));
}
function nodeSummary(nodes: NodeSpec[]) { return nodes.map((n,i) => ({ number: i+1, name: n.name, protocol: n.protocol, country: n.country, selected: !!n.selected })); }
async function dispatch(s: Service, args: string[]) {
  const [command, sub, ...rest] = args;
  const state = () => s.store.read();
  switch (command) {
    case 'help': case '--help': case '-h': out(help); break;
    case 'status': out(await s.status()); break;
    case 'doctor': { const r = await s.doctor(sub); out(r); if (r.probes.some(p => !p.healthy)) process.exitCode = 1; break; }
    case 'core':
      switch (sub) {
        case 'discover': out(await s.discoverSingBox()); break;
        case 'verify': case 'install': out(await s.core.prepare()); break;
        case 'use': out(await s.core.prepare(need(rest[0],'sing-box.exe'))); break;
        case 'start': out(await s.core.start()); break;
        case 'stop': await s.core.stop(); out('本工具内核已停止'); break;
        case 'restart': out(await s.core.restart()); break;
        case 'check': await s.store.lock(async () => s.core.check(await state())); out('sing-box check 通过'); break;
        default: throw new Error('未知 core 子命令');
      } break;
    case 'proxy':
      switch (sub) {
        case 'use-singbox': { const p=await s.useSingBox(need(rest[0],'name'),port(rest[1]),rest[2]); out({id:p.id,name:p.name,kind:p.kind});break; }
        case 'add-manual': { const p = await s.addManaged(need(rest[0],'name'),port(rest[1]),[manual(await json(need(rest[2],'node.json')))]); out({id:p.id,name:p.name}); break; }
        case 'add-subscription': {
          const source = await json(need(rest[2],'source.json')); const r = parse(await download(source.url));
          if (!r.nodes.length) throw new Error(`没有可用节点，不支持项 ${r.unsupported.length}`);
          const nodes = select(r.nodes, (source.selected || []).join(','));
          const p = await s.addManaged(need(rest[0],'name'),port(rest[1]),nodes,{kind:'subscription',url:source.url});
          out({id:p.id,name:p.name,unsupported:r.unsupported.length}); break;
        }
        case 'refresh': out(await s.refresh(need(rest[0],'id'))); break;
        case 'select': {
          const p = (await state()).profiles.find(p => p.id === rest[0]); if (!p?.nodes) throw new Error('该代理没有可选节点');
          await s.editProfile(p.id,{nodes:select(p.nodes,need(rest[1],'indexes'))}); out('节点选择已更新'); break;
        }
        case 'edit': { const patch = await json(need(rest[1],'patch.json')); await s.editProfile(need(rest[0],'id'),{ ...(patch.name ? {name:String(patch.name)} : {}), ...(patch.port ? {port:port(patch.port)} : {}) }); out('代理已更新'); break; }
        case 'remove': await s.removeProfile(need(rest[0],'id')); out('代理已删除'); break;
        default: throw new Error('未知 proxy 子命令');
      } break;
    case 'app':
      switch (sub) {
        case 'add': {
          let data:Parameters<Service['addApp']>[0];
          if(rest[0]==='codex'||rest[0]==='claude') {
            if(rest.slice(2).some(a=>a!=='--clone'))throw new Error('可选参数仅支持 --clone');
            const target=await s.apps.desktop(rest[0]);const binding=need(rest[1],'profileId|direct');
            const clone=rest.includes('--clone');
            data={...target,instance:clone?rest[0]:undefined,profileId:binding==='direct'?undefined:binding};
          }else data=await json(need(rest[0],'app.json'));
          const a=await s.addApp(data);out({id:a.id,name:a.name,guard:a.guard});break;
        }
        case 'bind': await s.apps.edit(need(rest[0],'id'),{profileId:need(rest[1],'profileId') === 'direct' ? undefined : rest[1]}); out('绑定已更新，下次启动生效'); break;
        case 'edit': { const p = await json(need(rest[1],'patch.json')); await s.apps.edit(need(rest[0],'id'),{...(p.name ? {name:p.name}:{}), ...(p.cwd?{cwd:p.cwd}:{}), ...(p.args?{args:p.args}:{})}); out('应用已更新'); break; }
        case 'remove': await s.removeApp(need(rest[0],'id')); out('登记已删除，应用数据保留'); break;
        default: throw new Error('未知 app 子命令');
      } break;
    case 'launch': out(await s.apps.launch(need(sub,'appId'))); break;
    case 'shortcut': if (!['create','remove'].includes(sub)) throw new Error('shortcut create|remove'); out(await shortcut(s.store,s.native,need(rest[0],'id'),sub==='remove')); break;
    case 'guard':
      switch (sub) {
        case 'enable': case 'disable': await s.guard.enable(need(rest[0],'id'),sub==='enable'); out('Guard 设置已更新'); break;
        case 'status': out({process:await s.guard.running(),events:await s.guard.eventsStatus(),protected:(await state()).apps.filter(a=>a.guard).map(a=>({id:a.id,name:a.name}))}); break;
        case 'events':
          if (rest[0] === 'status') out(await s.guard.eventsStatus());
          else if (['enable','disable'].includes(rest[0])) out(await s.guard.configureEvents(rest[0] === 'enable'));
          else throw new Error('guard events enable|disable|status');
          break;
        case 'start': out(await s.guard.start()); break;
        case 'run': { const abort = new AbortController(); process.on('SIGINT',()=>abort.abort()); process.on('SIGTERM',()=>abort.abort()); await s.guard.run(abort.signal); break; }
        default: throw new Error('未知 guard 子命令');
      } break;
    case 'settings': { const p = await json(need(sub,'settings.json')); await s.settings({...(p.doh?{doh:p.doh}:{}),...(p.testUrl?{testUrl:p.testUrl}:{}),...(p.exitUrl?{exitUrl:p.exitUrl}:{})}); out('设置已更新'); break; }
    case 'uninstall': if (!['all','core'].includes(sub)) throw new Error('uninstall all|core'); await s.uninstall(sub as 'all'|'core'); out('清理完成，应用数据与配置备份已保留'); break;
    default: throw new Error('未知命令；运行 help 查看用法');
  }
}
export async function menu(s: Service, prompt?: (message:string) => Promise<string>) {
  const rl = prompt ? undefined : createInterface({ input:process.stdin, output:process.stdout });
  const ask = async (message:string) => (await (prompt ? prompt(message) : rl!.question(message))).trim();
  const yes = async (prompt:string) => /^(y|yes|是)$/i.test(await ask(prompt + ' [y/N]：'));
  const pick = async <T extends {name:string}>(items:T[], prompt:string): Promise<T> => {
    if (!items.length) throw new Error('暂无记录，请先添加'); items.forEach((x,i)=>console.log(`${i+1}. ${x.name}`));
    const n=Number(await ask(prompt))-1; if (!Number.isInteger(n)||!items[n]) throw new Error('选择无效'); return items[n];
  };
  const binding = async () => {
    const profiles=(await s.store.read()).profiles;
    if(!profiles.length) {
      console.log('先准备应用要使用的代理，验证成功后继续添加应用。');
      return (await createProxy(false,true))?.id;
    }
    profiles.forEach((p,i)=>console.log(`${i+1}. ${p.name} (${p.port}${p.kind==='sing-box'?'，已有服务':''})`));
    console.log('n. 添加新的代理或订阅\n0. 直连（不启用 Guard）\nb. 返回');
    const answer=await ask(`选择应用要使用的代理${profiles.length===1?'（回车使用 '+profiles[0].name+'）':''}：`);
    if(answer.toLowerCase()==='b')throw new Error('已取消，返回主菜单');
    if(answer.toLowerCase()==='n')return (await createProxy(true))?.id;
    if(answer==='0')return undefined;
    const n=Number(answer||(profiles.length===1?'1':''));
    if(!Number.isInteger(n)||!profiles[n-1])throw new Error('请选择列表中的代理，或输入 n 新建');
    return (await readyProxy(profiles[n-1])).id;
  };
  const reuseExisting = async () => {
    console.log('正在查找并验证已有 sing-box 服务…');
    const found=await s.discoverSingBox();
    if(!found.available.length){console.log('未发现可复用的代理，接下来配置订阅或手动上游；sing-box 程序会优先使用本机安装，缺失时协助安装。');return undefined;}
    found.available.forEach((p,i)=>console.log(`${i+1}. ${p.host}:${p.port} (${p.version})`));
    const answer=await ask('使用已有代理（回车选 1，0 配置新的代理）：');if(answer==='0')return undefined;
    const chosen=found.available[Number(answer||'1')-1];if(!chosen)throw new Error('选择无效');
    const p=await s.useSingBox(`已有 sing-box ${chosen.port}`,chosen.port,chosen.host);
    out({id:p.id,name:p.name});return p;
  };
  const chooseNodes = async (nodes:NodeSpec[]) => {
    const countries=[...new Set(nodes.map(n=>String(n.country || 'Unknown')))];
    console.log('0. 全部地区'); countries.forEach((c,i)=>console.log(`${i+1}. ${c}`));
    const country=Number(await ask('国家/地区编号：')); if(!Number.isInteger(country)||country<0||country>countries.length)throw new Error('地区无效');
    const group=country===0?nodes:nodes.filter(n=>String(n.country||'Unknown')===countries[country-1]);
    out(nodeSummary(group)); const selected=select(group,await ask('节点编号（可多选，逗号分隔）：')).filter(n=>n.selected);
    const names = new Set(selected.map(n=>n.name)); if(names.size!==selected.length)throw new Error('所选节点重名，请修正订阅');
    return nodes.map(n=>({...n,selected:names.has(n.name)}));
  };
  const readyProxy = async (profile:Profile) => {
    console.log(`正在启动并验证代理「${profile.name}」…`);
    const ready=await s.prepareProfile(profile.id);
    console.log(`代理「${ready.name}」已就绪。`);
    return ready;
  };
  const createProxy = async (skipDiscovery=false,allowDirect=false):Promise<Profile|undefined> => {
    if(!skipDiscovery) {
      const existing=await reuseExisting();
      if(existing){console.log(`代理「${existing.name}」已验证，将直接使用。`);return existing;}
    }
    const type=await ask(`配置代理：1订阅（默认） 2手动上游${allowDirect?' d仅直连（不启用 Guard）':''} 0返回：`);
    if(type==='0')throw new Error('已取消，返回主菜单');
    if(allowDirect&&type.toLowerCase()==='d')return undefined;
    if(!['','1','2'].includes(type))throw new Error('请选择订阅或手动上游');
    const name=(await ask('代理名称（回车使用“我的代理”）：'))||'我的代理';
    const used=new Set((await s.store.read()).profiles.map(p=>p.port));
    let suggested=18099;
    while(suggested<=65535&&(used.has(suggested)||await tcp('127.0.0.1',suggested,150)))suggested++;
    if(suggested>65535)throw new Error('没有可用的本地监听端口');
    const listen=port((await ask(`本地监听端口（回车使用 ${suggested}）：`))||suggested);
    let p:Profile;
    if(type==='2') {
      const protocol=await ask('上游协议 http / socks5：');
      const server=await ask('上游地址：'); const server_port=port(await ask('上游端口：'));
      const username=await ask('用户名（可空）：'); const password=username?await ask('密码（仅存入受限数据目录，不打印摘要）：'):'';
      const useTls=protocol==='http'&&await yes('上游 HTTP 代理连接使用 TLS');
      p=await s.addManaged(name,listen,[manual({name,protocol,server,server_port,username,password,tls:useTls})]);
    } else {
      const url=await ask('订阅 URL：'); const r=parse(await download(url));
      console.log(`解析 ${r.nodes.length} 个节点，不支持 ${r.unsupported.length} 项`);
      if(r.unsupported.length)out(r.unsupported.map(n=>({protocol:n.protocol,reason:redact(n.reason)})));
      if(!r.nodes.length)throw new Error('订阅没有可用节点');
      p=await s.addManaged(name,listen,await chooseNodes(r.nodes),{kind:'subscription',url});
    }
    try {return await readyProxy(p);}
    catch(e:any){throw new Error(`代理配置已保存，但联网验证失败：${e.message}。修正配置后再添加应用`);}
  };
  const addApplication = async (boundProfileId?:string) => {
    const choice=await ask('选择应用：1 Codex  2 Claude  3 其他应用（手动指定）  0返回：');
    if(choice==='0')throw new Error('已取消，返回主菜单');
    if(!['1','2','3'].includes(choice))throw new Error('请选择 Codex、Claude 或其他应用');
    let data:Parameters<Service['addApp']>[0]|undefined;
    if(choice!=='3') {
      const kind=choice==='1'?'codex':'claude';
      const target=await s.apps.desktop(kind);
      console.log(`已找到 ${target.name} 桌面版，将自动跟随安装更新。`);
      const mode=await ask('使用方式：0 原版（默认）  1 空白分身：');
      if(!['','0','1'].includes(mode))throw new Error('使用方式无效');
      data={...target,instance:mode==='1'?kind:undefined};
    }
    const profileId=boundProfileId??await binding();
    if(!data) {
      const name=await ask('应用名称：'); const exe=(await ask('EXE 路径（可粘贴带引号路径）：')).replace(/^"|"$/g,'');
      const mode=await ask('启动方式：0普通应用 1Codex 空白分身 2Claude 空白分身（默认 0）：');
      if (!['','0','1','2'].includes(mode)) throw new Error('启动方式无效');
      const instance=mode==='1'?'codex':mode==='2'?'claude':undefined;
      const adapter=instance||await yes('是否确认该应用支持 Chromium/Electron --proxy-server 参数')?'chromium':'environment';
      const argsText=await ask('附加参数 JSON 数组（空白为 []）：');
      data={name,exe,adapter,instance,args:argsText?JSON.parse(argsText):[]};
    }
    console.log('Codex/Claude 桌面应用及其分身绑定代理后默认启用 Guard，首次或更新监听时需要 UAC 授权；之后可在 Guard 菜单停用。');
    const a=await s.addApp({...data,profileId});
    if(await yes('创建桌面快捷方式'))out(await shortcut(s.store,s.native,a.id));
    if(!a.guard && a.adapter==='chromium' && a.profileId && await yes('启用 Guard（误启动会关闭并代理重启，首次需要 UAC 授权监听）')){await s.guard.enable(a.id,true);a.guard=true;}
    out({id:a.id,name:a.name,guard:a.guard});
  };
  try {
    while(true) {
      console.log('\nApp Proxy — Windows\n1. 添加应用\n2. 管理/启动应用\n3. 添加代理或订阅\n4. 管理节点/刷新订阅\n5. sing-box 内核\n6. Guard 防误触\n7. 状态与诊断\n8. DNS/探测设置\n9. 卸载\n0. 退出');
      const choice=await ask('请选择：');
      try {
        if(choice==='0') break;
        switch(choice) {
          case '1': {
            await addApplication();break;
          }
          case '2': {
            const a=await pick((await s.store.read()).apps,'应用编号：');
            const action=await ask('1启动 2改绑定 3改名 4改参数 5创建快捷方式 6删除登记：');
            if(action==='1')out(await s.apps.launch(a.id));
            else if(action==='2')await s.apps.edit(a.id,{profileId:await binding()});
            else if(action==='3')await s.apps.edit(a.id,{name:await ask('新名称：')});
            else if(action==='4')await s.apps.edit(a.id,{args:JSON.parse(await ask('参数 JSON 数组：'))});
            else if(action==='5')out(await shortcut(s.store,s.native,a.id));
            else if(action==='6'&&await yes('删除此登记和快捷方式，保留应用数据'))await s.removeApp(a.id);
            break;
          }
          case '3': {
            const p=await createProxy();
            if(p&&await yes('代理已就绪，现在添加应用并使用此代理'))await addApplication(p.id);
            break;
          }
          case '4': {
            const p=await pick((await s.store.read()).profiles,'代理编号：');
            if(p.kind==='sing-box') {
              const action=await ask('已有服务：1诊断 2改名 3解除登记（保留原服务）：');
              if(action==='1')out(await s.doctor(p.id));
              else if(action==='2')await s.editProfile(p.id,{name:await ask('新名称：')});
              else if(action==='3'&&await yes('仅解除此入口登记'))await s.removeProfile(p.id);
              break;
            }
            const action=await ask('1查看节点 2选择节点 3刷新订阅 4改端口 5改名 6删除：');
            if(action==='1')out(nodeSummary(p.nodes||[]));
            else if(action==='2')await s.editProfile(p.id,{nodes:await chooseNodes(p.nodes||[])});
            else if(action==='3') {
              try{out(await s.refresh(p.id));}catch(e:any){
                if(!String(e.message).includes('清空')||!p.source?.url)throw e;
                console.log(e.message); if(await yes('重新下载并选择节点')) {const r=parse(await download(p.source.url));await s.editProfile(p.id,{nodes:await chooseNodes(r.nodes)});}
              }
            }
            else if(action==='4')await s.editProfile(p.id,{port:port(await ask('新端口：'))});
            else if(action==='5')await s.editProfile(p.id,{name:await ask('新名称：')});
            else if(action==='6'&&await yes('删除此代理'))await s.removeProfile(p.id); break;
          }
          case '5': {
            console.log('启动、停止和重启仅作用于本工具配置的实例，已有服务保持由原启动器管理。');
            const action=await ask('1启动 2停止 3重启并重新探测网卡 4检查配置 5查找或安装程序 6指定已有程序 7发现已有服务：');
            if(action==='1')out(await s.core.start());
            else if(action==='2')await s.core.stop();
            else if(action==='3')out(await s.core.restart());
            else if(action==='4'){await s.store.lock(async()=>s.core.check(await s.store.read()));out('配置检查通过');}
            else if(action==='5')out(await s.core.prepare());
            else if(action==='6')out(await s.core.prepare((await ask('sing-box.exe 路径：')).replace(/^"|"$/g,'')));
            else if(action==='7')out(await s.discoverSingBox());break;
          }
          case '6': {
            const action=await ask('1启用应用保护（自动授权监听） 2停用应用保护 3修复监听授权 4撤销监听授权 5查看状态：');
            if(action==='1'||action==='2') {
              const a=await pick((await s.store.read()).apps,'应用编号：');
              await s.guard.enable(a.id,action==='1');
            } else if(action==='3'||action==='4') {
              console.log('Windows 可能请求 UAC 授权；只有监听辅助进程提权，Guard 和目标应用保持原权限。');
              out(await s.guard.configureEvents(action==='3'));
            } else if(action==='5') out({process:await s.guard.running(),events:await s.guard.eventsStatus()});
            break;
          }
          case '7': out(await s.status()); if(await yes('执行真实代理和出口探测'))out(await s.doctor()); break;
          case '8': {
            const action=await ask('1自定义 DoH 2探测 URL 3出口 IP URL：');const url=await ask('URL：');
            await s.settings(action==='1'?{doh:url}:action==='2'?{testUrl:url}:{exitUrl:url});break;
          }
          case '9': {const action=await ask('1清理全部登记和工具资源 2仅清理内核（保留配置）：');if(['1','2'].includes(action)&&await yes('确定执行，应用数据和备份会保留'))await s.uninstall(action==='1'?'all':'core');break;}
          default: console.log('请选择有效编号');
        }
      } catch(e:any){console.error('操作未完成：'+redact(String(e.message)));}
    }
  } finally { rl?.close(); }
}

if(process.argv[1]&&resolve(process.argv[1]).toLowerCase()===fileURLToPath(import.meta.url).toLowerCase()) {
const args=process.argv.slice(2); let home:string|undefined; let notify=false;
for(let i=0;i<args.length;){if(args[i]==='--home'){home=need(args[i+1],'home');args.splice(i,2);}else if(args[i]==='--notify'){notify=true;args.splice(i,1);}else i++;}
if(args[0]==='help'||args[0]==='--help'||args[0]==='-h'){out(help);}else{
  const service=new Service(home);
  try{await service.init(); if(args.length)await dispatch(service,args);else if(process.stdin.isTTY)await menu(service);else out(help);}
  catch(e:any){const message=redact(String(e.message));console.error(message);process.exitCode=1;await service.store.log('error',message).catch(()=>{});if(notify)await service.native.call('alert',{message}).catch(()=>{});}
  finally{service.close();}
}
}
