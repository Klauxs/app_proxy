# App Proxy for Windows

通过独立启动入口给桌面应用绑定代理，优先复用已有 sing-box 服务或程序，缺失时协助安装。支持自有配置的自动物理网卡选择、多出口、订阅节点选择及 Guard 防误触。界面是中文终端菜单，没有桌面 GUI。

## 启动

推荐使用 `release/` 下的最新便携包：解压到固定目录，双击 **App Proxy.cmd**。包内附 Node.js 24 和 sing-box 1.14.1 x64（含原版配套 DLL、许可证及源码链接），无需另装内核、Node 或 PowerShell 7。Windows 集成使用系统 Windows PowerShell 5.1。

源码运行需要 Node.js 24 或更高版本：

```powershell
cd D:\app_proxy\windows
node src/cli.ts
```

快捷方式和 Guard 登录任务引用当前工具目录；建立这些入口后请勿直接移动目录。如需迁移，先关闭 Guard、删除工具快捷方式，再在新目录重新创建。

## 首次使用

1. 选择 **1「添加应用」→ Codex / Claude / 其他应用**。Codex、Claude 自动从当前用户的 Windows 应用包登记定位主程序，接着选择原版（回车默认）或空白分身，不用填写名称、EXE、适配类型和附加参数。未找到桌面版会提示安装，或改选“其他应用”手动指定。
2. 然后选择代理配置（只有一个可回车）；没有时自动发现并验证已有 sing-box HTTP/mixed 入口。没有可复用入口或选择配置新的代理时，就在当前流程填写订阅或手动 HTTP/SOCKS5 上游。订阅继续选择地区和节点；代理名称和空闲监听端口提供默认值，回车即可。需要 sing-box 程序时优先使用本机已有程序，其次随包副本，均缺失时协助安装。
3. 工具启动自建实例并验证选中的代理；自建实例按 Wi-Fi、其他物理网卡尝试，已有服务只验证，不改配置或生命周期。成功后自动绑定刚准备好的代理，不再询问“代理编号”。
4. Codex、Claude 直接完成登记；只有“其他应用”需要填写名称、实际 EXE 和适配参数。之后按需创建桌面快捷方式。多个代理配置可以使用不同端口和出口。
5. Codex、Claude 桌面应用及其分身在绑定代理后默认启用 Guard，首次会请求 UAC；其他确认支持 `--proxy-server` 的 Chromium/Electron 应用仍询问是否启用。普通程序选择环境变量适配，不默认传入 Chromium 参数。

也可以先选择 **3「添加代理或订阅」**，验证成功后接着选择“现在添加应用并使用此代理”。没有配置时不再显示只有“0 直连”的代理编号列表；直连必须显式选择，取消会返回菜单。代理验证失败时保留已保存的代理配置供修改，不继续添加应用或启用 Guard。

日常双击创建的 `App Proxy - <ID>.lnk` 即可。代理不可用时会弹窗报错，不会自动改为直连。启动成功只说明创建了进程，不能据此认定所有子进程和协议均经过代理。

命令行也可使用 `node src/cli.ts app add codex <代理ID>` 或 `app add claude <代理ID>`，附加 `--clone` 创建空白分身，`direct` 表示明确直连。高级用户仍可使用 `app add <app.json>`。内置识别使用应用包身份和主入口，实际 EXE 从登记读取：Codex 的主程序也可能叫 `ChatGPT.exe`，不会因此认成普通 ChatGPT；升级后继续按包身份解析新路径。

## sing-box 的复用与安装

程序与实例分开处理：已有服务验证监听进程、程序版本及真实代理请求后直接复用，不导入或覆盖其配置，也不启动、停止或重启它。创建自己的配置时，复用现有 sing-box 程序，另开一个使用工具配置的实例；只有这个实例的生命周期归工具管理。

自动查找 PATH、正在运行的 sing-box 程序以及 Scoop/WinGet 常见入口，也可用 `core use <exe>` 指定。已有程序验证 `version`，配置用该程序实际执行 `check`；不要求与随包版本相同，不自动升级用户的安装。无法找到已有程序时，依次使用之前下载的程序、随包副本；均不可用才从固定官方制品下载，验证 ZIP 和 EXE/DLL 的 SHA256，安装到数据目录 `bin/sing-box-1.14.1`。

`kind: managed` 保存工具自己的节点、订阅和入口；`kind: sing-box` 只保存已有本机服务的地址和名称，节点列表为空。复用服务只接受可识别 sing-box 进程的无认证 HTTP/mixed 回环入口；不恢复通用 `external` 类型，也不兼容旧 external 数据。进程路径不可读或只有 SOCKS/TUN 入口时不会自动认领。

应用入口首版使用无认证 HTTP 代理。有认证的 HTTP/SOCKS5 上游通过托管 sing-box 转接。多个应用绑定相同端口，会共享该端口对应的出口。

## 订阅与节点

迁移自随项目提供的 macOS JXA：

- 下载顺序使用 Clash.Meta、Loon、Quantumult X、Surge、Shadowrocket 等客户端标识，下载成功即进入解析。
- 识别原解析器覆盖的 Clash YAML、Loon/Surge/Shadowrocket/Quantumult X 风格配置、URI 和 Base64 节点列表。
- 节点类型：AnyTLS、VLESS、VMess、Shadowsocks、Trojan、Hysteria2/hy2。
- 按原规则根据节点名和旗帜分组；可选择全部地区，或单个国家/地区，再多选节点。
- 多选生成 `urltest` 组，由 sing-box 执行测速和选择。
- 手动刷新按节点名保留选择，报告新增、移除；不能确定选择或更新会清空出口时保留旧配置。菜单可重新下载、选择。
- 未识别格式及非法节点会显示数量和原因类别，不把未知内容当完整 sing-box 配置执行。

这不是通用 YAML/客户端配置解析器，不保证所有扩展字段。六类协议已做解析及真实内核配置检查；真实服务器连接兼容性仍需对应节点验证。

已修复原版把块状 ALPN 列表误当节点的问题。用户提供的真实 Clash 订阅现已解析为 85 个 AnyTLS 节点、0 个不支持项；其中一个美国节点通过实际 HTTPS 和出口探测。

## Codex / Claude 分身与 MSIX

如需代理原来的安装版，在“添加应用”选择 **0 普通应用**，为 Claude 选择 Chromium/Electron 适配并绑定代理。该模式不设置分身数据目录，不创建 `claude-home`，沿用 Claude 原有数据；已运行时拒绝重复启动，需要先退出原版。启用 Guard 后，无代理启动的原版会被关闭并代理重启，已登记的分身单独识别。Claude 2.2553.1.0 原版模式的实机证据见 TEST-RESULTS.md。

“添加应用”可选择普通应用、Codex 空白分身或 Claude 空白分身；JSON 登记使用 `"instance":"codex"` 或 `"instance":"claude"`。两种分身都使用 `instances/<应用ID>/user-data`，通过 `--user-data-dir` 指定独立用户数据目录。

| 分身 | 额外目录与环境变量 |
|---|---|
| Codex | `codex-home` → `CODEX_HOME`，`user-data` → `CODEX_ELECTRON_USER_DATA_PATH` |
| Claude | `claude-home` → `CLAUDE_CONFIG_DIR` |

只为本次进程及继承环境的子进程设置这些值，不修改 settings 文件、系统环境或应用安装内容，不复制登录状态。Claude 的目录变量供遵循该变量的 Claude Code 组件使用，不能据此认定 Desktop 的所有组件都会采用它。[Claude Code 环境变量说明](https://code.claude.com/docs/en/env-vars)

Claude 示例见 `examples/claude-instance.json`。两种分身共用代理启动、快捷方式、MSIX 启动及按实例识别的 Guard。本机 Claude MSIX 2.2553.1.0 已验证原版与分身共存、独立目录初始化，以及分身网络进程实际连接内置代理。该版本拒绝远程调试参数，未使用 CDP 验证出口正文；登录、模型对话、Code/Cowork 仍未验收。

工具自动读取包清单的文件虚拟化设置。未明确全局关闭 AppData 文件虚拟化的 MSIX，启动请求、回执和分身目录改放在 `%LOCALAPPDATA%\Packages\<包家族>\LocalState\AppProxy\<工具数据目录摘要>`，按工具数据目录和应用 ID 区分；已分配的位置在包更新后保留。Claude 属于这种情况，Codex 当前版本关闭虚拟化，继续使用原工具目录。这是按包清单处理，不按应用名称特判。LocalState 属于应用包数据，Windows 卸载或重置原包可能清除其中分身数据。[微软 MSIX 存储说明](https://learn.microsoft.com/en-us/windows/msix/msix-troubleshooting-guide)

已在本机 Codex MSIX 26.911.7940.0 验证原实例与分身同时运行，分身独立初始化 Codex 数据库和配置目录。在分身浏览器上下文访问出口端点返回 HTTP 200、出口 `64.118.152.127`，实际连接为 `127.0.0.1:18099`。这证明该次浏览器请求经过指定代理，不代表所有协议或模型对话均已验证。

选择 WindowsApps 中已登记的 EXE 时，工具自动查询当前用户的包清单，按 EXE 匹配包家族和应用 ID；没有写死 Codex 包名。启动时根据保存的包身份重新解析当前安装路径，使用 Windows 的包上下文命令启动辅助程序，再由辅助程序设置本次环境和启动目标应用，避免直接执行包内 EXE 导致 EPERM。只支持登记为 Windows.FullTrustApplication 的桌面应用；不更改包文件、签名、ACL、系统代理或持久环境变量。

自动识别发生在用户指定 EXE 之后，目前没有全量 MSIX 应用选择列表。通用包识别/启动独立于分身类型；Codex 和 Claude 的目录环境变量分别设置，并由同一 MSIX 辅助程序传入目标进程。

工具快捷方式使用原应用 EXE 的完整图标资源（保留多尺寸和原始色深），提取到工具数据目录的 `icons/<应用ID>.<内容摘要>.ico`，避免 MSIX 更新移除旧版本目录后图标失效。重新创建快捷方式会从当前安装版本刷新图标；图标内容变化会改变缓存引用，保存后通过 SHChangeNotify 通知桌面刷新。启动目标仍是工具启动器和应用 ID，MSIX 每次启动按包身份解析最新 EXE；普通 EXE 更新后路径不变也可继续用，改变路径则需重新登记。移动或删除工具目录仍会使其快捷方式失效；没有实际升级验收时，不保证所有更新器行为。

如果工具从 MSIX 包进程中运行，缓存文件可能被重定向到包目录。写入 IconLocation 时通过文件句柄解析真实磁盘路径，保证包外 Explorer 也能读取；不能仅凭包内文件存在或包内 Shell 解码成功判定桌面图标正常。

底层使用微软公开的 `Invoke-CommandInDesktopPackage -PreventBreakaway`。微软将其定位为诊断工具，仅保证包身份及虚拟化资源访问；因此兼容性以实际 Windows/应用版本验收为准，不宣称适用于所有 MSIX 或 AppContainer 应用。[微软接口说明](https://learn.microsoft.com/en-us/powershell/module/appx/invoke-commandindesktoppackage)

Guard 按分身目录识别目标，实际 Codex 和 Claude 无代理误启动均已验证能被关闭并代理重启，原实例保持存活。工具删除登记、卸载默认保留分身数据。各实例仍属于同一个 Windows 用户，包级凭据和系统资源不构成安全隔离。没有复制用户登录文件。

单个 MSIX 应用被卸载或暂时无法查询时，Guard 记录该应用的错误并继续检查其他登记。

## Guard 防误触

Guard 的包查询和进程扫描在配置锁外执行，真正纠正前重新核对配置快照；点击启动不会再被一次完整的后台扫描阻塞。进程所属用户通过 Windows 令牌查询，同时核对创建时间和会话，不再为每个进程单独调用 WMI GetOwnerSid。已有配置的初始化也不再无条件获取写锁。

在应用上显式启用后，会注册当前用户登录任务并运行一个后台 Guard。也可以随时停用。

添加时，内置 Codex/Claude 桌面入口和分身绑定代理后默认启用 Guard；菜单和 `app add` 行为一致，不再额外询问是否启用。手动登记的 Chromium 应用按主程序包身份或 `Codex.exe` / `Claude.exe` 名称识别，不使用可编辑的显示名；环境变量适配的同名 CLI、包内辅助入口和直连应用不自动启用。`app.json` 可显式设置 `"guard":false` 跳过；之后可用 **6 Guard → 2 停用应用保护** 关闭。不批量修改已有登记。

其他应用可在 **6 Guard → 1 启用应用保护** 中选择，首次启用自动请求 Windows UAC 授权。取消授权不会启用新的保护；添加流程保留应用登记并明确提示 Guard 未完成，可稍后重试。只有通知辅助进程以管理员权限运行，Guard 和目标应用不因此提权；请使用当前 Windows 账号授权，该账号须属于管理员组。启用时会刷新普通权限 Guard 及登录任务，让新版监听接入。菜单“3 修复监听授权”仅用于修复或更新。

授权把固定监听脚本安装到 `%ProgramFiles%\AppProxyGuardEvents`，目录、脚本及任务仅管理员可修改；普通 Guard 只能运行任务、接收当前会话的进程通知，监听端不执行任何客户端命令。每次登录后，原有 Guard 登录任务启动 Guard，再由它按需启动已授权的监听任务，不重复弹 UAC。监听与 Guard 的连接关闭后自动退出；全部停用保护会停止后台进程，但保留授权，方便下次启用。菜单 **6 → 4** 可撤销授权，全部卸载也会撤销（可能再次需要 UAC）。监听程序更新需要重新授权，不会静默替换管理员代码。

- 启动时扫描一次；直接订阅 ETW 进程启动事件，每 100ms 主动刷新事件缓冲，收到已登记 EXE 的通知后立即检查同一用户、同一交互会话内的真实进程。事件模式每 30 秒扫描补漏。
- 监听统一使用管理员辅助进程，已删除普通权限监听路径。授权缺失、监听失败或中断时，后台临时退回约每 2 秒扫描，每 30 秒尝试恢复，不反复弹 UAC。`events.log` 中的 `guard-events-ready elevated-etw-process-start` 表示已连接 ETW 管理员监听，`guard-events-fallback` 标明降级原因。
- 检查预期代理参数及冲突参数，排除带 `--type` 等标识的辅助进程。
- 检测到误启动时请求正常关闭，1.5 秒后仍存活则重新核验身份并终止，再由代理启动器拉起。
- 同时核验路径、PID、创建时间与用户，避免按同名 EXE 批量结束进程。
- 不再固定等待 1.2 秒；进程或参数暂不可读时最多追加三次短暂重试，不把空参数当成无代理。纠正仍有至少 5 秒冷却、每分钟最多三次限制。代理不可用或仍有辅助进程时不强行拉起，原因写入 `events.log`。
- Guard 可用于绑定自建配置或已有 sing-box 服务的 Chromium 应用；纠正应用前验证代理，已有服务仍由原启动器管理。
- 保存新代理绑定后，已由工具启动的原实例保留原绑定，下一次启动使用新绑定。

Guard 首版仅对确认支持 Chromium 参数的应用开放，已验收上述 Codex MSIX 分身。只读取环境变量的通用程序、其他未经验证的 MSIX 及特殊单实例机制不宣称兼容。启动事件也发生在进程创建之后，纠正前仍有直连时间窗口；重启可能中断该应用未保存的工作。开发测试不会自动对用户现有原实例启用保护。

当前事件源为 `Microsoft-Windows-Kernel-Process` 的 ProcessStart，通过 TDH 按字段名解析，兼容事件 payload 版本变化。只创建本工具的实时追踪会话，不写 ETL、不更改系统审计策略；退出、升级和卸载清理自己的会话，下次启动也能恢复强制终止留下的会话。解码失败或 ETW 报告丢失事件时通知 Guard 降级。

本机两轮各 8 次实测，请求启动到收到通知分别为 12–117ms、17–132ms（旧 WMI 基线 1304–1999ms）；32 个短进程通知全部收到。100ms 是主动刷新间隔，不是延迟上限，也不包含 Guard 的身份检查、停止和重启耗时。完整证据见 `TEST-RESULTS.md`。升级旧版后通过 **6 Guard → 3 修复监听授权** 更新管理员脚本并刷新 Guard，需再确认一次 UAC。

## 命令行

```powershell
node src/cli.ts help
node src/cli.ts --home D:\AppProxyData status
node src/cli.ts --home D:\AppProxyData core verify
node src/cli.ts core discover
node src/cli.ts core use D:\Tools\sing-box\sing-box.exe
node src/cli.ts proxy use-singbox Existing 7890
node src/cli.ts --home D:\AppProxyData proxy add-manual MyExit 18099 examples\manual-node.json
node src/cli.ts --home D:\AppProxyData app add examples\app.json
node src/cli.ts --home D:\AppProxyData launch <appId>
node src/cli.ts --home D:\AppProxyData guard enable <appId>
node src/cli.ts --home D:\AppProxyData doctor
```

`<appId>` 替换为实际 ID；运行前修改示例中的占位路径及绑定。便携包可将 `node` 换成 `runtime\node.exe`。包含订阅 URL 或凭据的命令使用 JSON 文件输入，避免放到进程命令行中。状态摘要隐藏节点密码、订阅 URL 和应用参数。

`adapter: environment` 只设置子进程代理变量，程序是否使用取决于其网络库。`adapter: chromium` 另外设置代理参数。直连会去掉子进程的 HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY；Chromium 适配器还会传 `--no-proxy-server`。不改父进程环境、用户持久环境或系统代理。

## DNS 与诊断

默认保留原版 Google、Cloudflare、AliDNS DoH 定义；自定义 DoH 排在首位，主解析使用 IPv4-only，DoH 域名由本地 DNS bootstrap。预置备用定义不等于已经实现故障自动切换。

对自建配置，启动、重启或更新时读取 Windows 网卡属性、IPv4 地址和默认路由；优先 Wi-Fi，再按 metric 选择其他物理网卡，排除 TUN/TAP、VPN、回环、蓝牙和虚拟网卡。应用冷启动验证其绑定出口，修改节点验证被修改的配置，通用启动要求至少一个自建出口成功。候选失败后尝试其他物理网卡，最后回退到 `route.auto_detect_interface=true`，状态和日志标明可能经过虚拟网卡。复用已有服务不改变其网卡或 DNS 设置。

这是尽量避开 TUN 的出站选择，不是禁止所有虚拟网卡的强制策略。运行期间不持续监听网卡变化；网络切换后用“重启并重新探测网卡”。DoH 的系统 bootstrap、订阅下载属于另外的请求路径，不承诺完全绕开其他代理的 DNS/WFP 拦截。[sing-box 路由字段](https://sing-box.sagernet.org/configuration/route/)

默认健康端点为 `https://www.gstatic.com/generate_204`，出口 IP 端点为 `https://api.ipify.org`，都可在菜单中修改。探测本身由工具发起，不能证明目标应用已经采用代理。企业环境可换成自己控制的健康端点。

诊断区分：进程身份、入口可连接、代理请求成功、出口站点结果和目标进程启动记录。HTTPS 验证证书，不用中间人解密验收。普通 TCP/HTTP 监听不能通过默认 HTTPS CONNECT 探测。

## 数据与卸载

初始化时通过配置文件句柄解析真实存储目录；快捷方式、后台进程和登录任务的 `--home` 使用这个物理路径，避免从 MSIX 包内创建入口后，包外桌面读取另一份空配置。`.store-scope.json` 保留原目录标识，用于保持 Guard 任务名称及 MSIX 分身目录稳定；仍携带旧逻辑路径的运行中分身也会被识别。此过程不移动或复制用户配置。`status` 的 dataDirectory 显示实际使用的目录。

默认入口首次初始化后，将实际路径记录到 `%USERPROFILE%\.app-proxy-home.json`；中文菜单和不带 `--home` 的 CLI 共用此记录，包内外读取相同配置。显式 `--home` 或 `APP_PROXY_HOME` 仍可指定独立目录，不改默认记录。配置互斥使用 Windows 独占文件句柄，辅助进程退出时由系统释放，不再根据 PID 删除旧锁目录。

默认 `%LOCALAPPDATA%\AppProxy`，可用 `--home` 或 `APP_PROXY_HOME` 指定。新建数据目录限制为当前用户和 SYSTEM 访问；首版配置及必要的节点凭据在此目录中保存为明文 JSON，不上传。原始 sing-box 流量日志默认关闭，工具事件日志脱敏并轮转。

配置更新先 `sing-box check`，保留一份配套配置备份；运行时更新若重启或代理请求失败，会恢复旧配置。新建配置的 `check` 通过不代表上游可用，启动后需诊断。

应用冷启动仅要求它绑定的代理出口可用；直接启动/重启内核要求至少一个出口可用，其他失效节点可用 doctor 查看。运行时修改节点或端口仍须验证被修改的配置，失败则回滚，避免无关的失效配置阻止正常出口使用。

- 删除一个应用：关闭其保护、撤销快捷方式和登记，不删除原程序与数据。
- 仅清理内核：停止本工具内核，关闭依赖该内核的应用 Guard，清除运行登记，保留代理配置、应用登记及包内程序。
- 清理全部：关闭 Guard、移除本工具登录任务和快捷方式、停止托管内核、清空应用及代理登记。保留配置备份、日志和应用数据。
- 已安装、下载或随包的 sing-box 程序均保留；已有服务的进程与配置保持不动。仅清理内核不关闭绑定已有服务的 Guard。

若用户修改了快捷方式目标/参数，工具会拒绝删除该快捷方式并报告。没有自动递归清除数据目录的选项。

## 开发与验证

```powershell
npm ci --ignore-scripts
npm run setup-core
npm run check
npm test
npm run package
```

`setup-core` 是开发/打包阶段的准备步骤：从固定的官方制品（或 `.tools/sing-box.zip` 缓存）校验 SHA256 后提取内置程序，用户运行便携包无需下载。测试使用同一内置内核。HTTPS 测试使用本机 Git 附带的 OpenSSL 在测试目录生成一天有效的临时证书，不写入系统证书存储。

测试会在 `.test-data` 创建隔离目录和受控 EXE，短暂创建其专属桌面快捷方式及登录任务，结束后撤销任务并停止测试进程。测试证据保留在该目录。详见 `TEST-RESULTS.md`。

`scripts/migrate-legacy.ts` 可重新生成 `src/legacy` 两个模块。Windows 改动包括移除 JXA I/O、使用 Node Base64、修复块状 ALPN 列表、替换被新内核移除的 block 出站为 reject 路由。保留原版自动绑定回退；Windows 物理网卡探测由 `network.ts` 和原生桥接实现，`core.ts` 写入实际选择。

## 已知范围

- Codex 上述版本的双开、目录初始化、浏览器代理请求及 Guard 已验证；Claude 2.2553.1.0 已验证双开、目录初始化和网络进程连接代理。没有执行登录后的模型对话，也未通过实际应用升级验收；详见 TEST-RESULTS.md。
- 尚未实现设备指纹、VPS 自动部署、TUN/WFP、定时订阅刷新或内核自动更新。
- 内置内核及便携包为 Windows x64；其他架构未验收。
- PowerShell 被组织策略禁用、VBS/WSH 不可用时，快捷方式隐藏启动可能受限；CLI 仍会明确报告相关失败。

## 来源

- `../AppProxyInstaller-scripts/`：用户提供的原版实现，订阅与配置逻辑的迁移来源。
- sing-box 官方发布：https://github.com/SagerNet/sing-box/releases/tag/v1.14.1
- 便携包附带 Node.js 的 LICENSE；sing-box 由用户选择现有程序或从官方制品安装。
