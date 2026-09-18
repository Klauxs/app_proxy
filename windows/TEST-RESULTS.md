# Windows 首版验证记录

日期：2026-09-18（Asia/Shanghai）

## 隐藏 MSIX 辅助进程（最新）

- `npm run check` 和 6 项 MSIX 回归通过。
- 本机 Codex、Claude 的实际 `Invoke-CommandInDesktopPackage` 入口均通过 `wscript.exe`、`hidden.vbs` 启动 Node 辅助进程，成功返回 `LAUNCH_EXPIRED` 回执。使用隔离过期请求验证链路，没有启动或关闭用户应用；未进行完整目标应用启动的肉眼闪窗验收。

## 快捷方式命名（最新）

- 最终命名：原版 `Claude.lnk` / `Codex.lnk`；分身 `Claude - <8位ID>.lnk` / `Codex - <8位ID>.lnk`，不再自动添加“分身”文字。此调整后类型检查及 9 项菜单流程回归通过。
- `npm run check` 通过。使用真实 Windows 快捷方式在隔离目录验证原版名称、旧名称迁移、分身 hash 后缀、同名文件保护和登记后删除；均通过。
- 已更新当前安装目录的命名实现，将用户桌面旧快捷方式改为 `Claude.lnk`，确认原快捷方式消失、启动目标存在且继续使用 Claude 缓存图标。未启动或结束用户应用。
- 此次为快捷方式局部修改，未重跑全量代理和 Guard 测试；此前完整测试结果见下文。

## Codex / Claude 内置添加入口（最新）

- `npm run check` 通过；完整 `npm test` 54 项通过，0 失败/跳过，约 147 秒。
- 新增回归覆盖两种应用 × 原版/分身的简化提问、没有安装时不创建代理、不读取 PATH 同名 CLI、更新后使用最新登记，以及 Codex 实际 EXE 为 ChatGPT.exe 时仍按主包身份默认启用 Guard。
- 实机只读发现：Codex 包 `OpenAI.Codex_2p2nqsd0c76g0`、主 AppId `App`，当前实际 EXE 为 `app/ChatGPT.exe`；Claude 包 `Claude_pzs8sxrjxfjjc`、主 AppId `Claude`，实际 EXE 为 `app/Claude.exe`。Claude 的 `SshAskpass` / `SshProxy` 辅助入口不参与选择。
- 隔离目录 `.test-data/preset-cli-iM53PT` 执行四种 CLI 添加组合：`app add codex|claude direct`，以及附加 `--clone`。四条登记的包身份、Chromium 适配和分身类型均正确。显式 direct 均保持 Guard 关闭；没有启动或结束用户应用，没有修改用户现有登记或触发 UAC。
- 菜单仍在代理准备和验证成功后才保存应用并自动启用 Guard；“其他应用”保留手动 EXE 和适配参数入口。用户实际代理和当前 Codex/Claude 会话未用于破坏性重启测试。

## 先代理、后应用的连续引导

- `npm run check` 通过；完整 `npm test` 50 项通过，0 失败/跳过，约 148 秒。
- 新增 7 项菜单流程测试：无配置时创建手动代理并继续、发现可复用服务直接绑定、单一配置回车选择、联网失败/取消不登记应用、显式直连、独立新增代理后继续添加、订阅下载选节点后继续。其中订阅使用本机受控 HTTP 服务，配置/授权调用由测试替身记录顺序，没有请求用户订阅。
- 真实 sing-box 集成测试通过 `prepareProfile` 从未启动状态准备指定入口并联网，再执行双出口、HTTPS、应用启动、Guard、回滚和清理；已有服务集成测试通过该入口验证服务，确认未生成自建配置、未认领或停止独立服务。
- CLI help 入口检查通过。菜单交互通过实际 menu 函数注入回答验证；未操作用户现有应用、代理数据或 UAC 授权。
- 没有代理时引导配置并提供默认名称/空闲端口，成功后沿用新配置，不再显示只有直连项的“代理编号”。验证失败保留代理配置供修正，取消返回菜单，均不继续登记应用。

## Codex / Claude 默认 Guard

- `npm run check` 通过。新增 `default-guard.test.ts` 三项通过：原版与分身的默认保护持久化、直连/环境变量/其他 EXE/显式关闭不触发授权、取消 UAC 保留登记且保护关闭并报错。
- `guard-events.test.ts` 四项回归通过，包括现有授权取消行为和扫描降级。此改动未重新执行整套端到端测试；上轮 ETW 的完整及实机验证见下节。
- 自动测试通过模拟授权和后台启动来避免操作真实用户应用；没有自动开启已有应用的 Guard。

## ETW 通知验证

- 监听源改为 Microsoft-Windows-Kernel-Process / ProcessStart，100ms 主动刷新；原 UAC 任务、受保护脚本和只传通知的管道继续复用。不新增驱动、第三方库或系统审计配置。
- `npm run check` 通过；完整 `npm test` 39 项通过，0 失败/跳过，约 147 秒。随后新增 ETW 丢失事件错误传递用例，相关文件 6 项全部通过，当前共 40 项用例；没有把静态检查当成实机验证。
- 通过 `node tests/elevated-events-live.ts` 单独运行真实 UAC 测试，全部使用临时授权任务与受控 Node 子进程，不操作用户登记的应用。两轮均验证普通客户端、管理员通知端、脚本禁止普通用户写入、任务用户权限仅读/运行、断开重连不重复 UAC。

| 请求启动 → Node 客户端收到通知 | 样本数 | 范围 | 中位数 |
|---|---|---|---|
| 原 WMI 基线 | 8 | 1304–1999 ms | 1656 ms |
| ETW 第一轮 | 8 | 12–117 ms | 84 ms |
| ETW 第二轮 | 8 | 17–132 ms | 91.5 ms |

- 第二轮同时运行自动回归；额外并发启动 32 个短进程，32/32 收到正确 PID 和程序名。查询本工具 ETW 会话显示 Buffers Lost = 0。该有限样本不代表所有负载下零丢失。
- 10,011ms 采样期间，监听辅助进程 CPU 时间为 46.875ms，约单核 0.468%；不包含全部 ETW 内核/系统开销。生产监听检查 ETW 丢失计数，遇到丢失或解码错误则让 Guard 降级扫描。
- 强制停止测试监听任务后重连，成功回收同名且 GUID 匹配的残留会话并继续收到新进程通知；撤销临时任务及脚本后确认 ETW 会话已移除。
- 通知数据的跨 API 时间戳存在约 1–9ms 差异，不把负的“回调到客户端”差值解释为真实负延迟。上表总耗时的起止均取 Node 客户端时钟；100ms 是刷新间隔，不是延迟上限。
- 追踪会话正常退出时清理，升级/撤销授权时也清理；不触碰其他软件的 ETW 会话。安装程序和监听脚本仍要求当前管理员账号授权。
- 可版本化的脱敏摘要：[tests/evidence/etw-2026-09-18.json](tests/evidence/etw-2026-09-18.json)。本机原始证据：`.test-data/events-live-YmSG79/`、`.test-data/events-live-VifTFv/`，后者 `cleanup.json` 为 `error=null, removeExit=0, traceRemoved=true`。
- 本次未更新用户已有安装，未重启电脑，未测用户应用的完整误启动纠正耗时。已安装旧版需从新版菜单“6 Guard → 3 修复监听授权”更新高权限脚本并刷新 Guard。

## 默认提权与 WMI 事件延迟定位（历史基线）

- 启用应用保护和前台启动 Guard 时自动完成管理员监听授权，不再要求单独选择。取消 UAC 不改变原应用保护设置；后台只重连已授权任务，不弹 UAC。删除普通权限监听实现；监听故障时仅保留扫描兜底。
- 启用保护时刷新旧 Guard 进程及普通登录任务，防止仍复用不支持提权监听的旧代码。测试覆盖授权先于保存、取消授权、停用不提权、无效应用不申请授权，以及缺少授权时不走普通监听。
- 真实延迟测试使用单独的临时最高权限任务，订阅准备好后启动 8 个 Node 受控进程，记录 OS 创建时间、WMI `TIME_CREATED`、C# 回调时间与 Node 客户端接收时间。该测试不运行 Guard 扫描和纠正循环，也不做目标应用重启。

| 测量区间 | 8 个样本结果 |
|---|---|
| 请求启动 → OS 创建进程 | 2–4 ms，中位数 3 ms |
| OS 创建 → WMI 生成通知 | 1301–1996 ms，中位数 1653 ms |
| WMI 生成 → C# 回调 | 约 0–0.5 ms |
| C# 回调 → Node 客户端 | 约 0–3 ms |
| 请求启动 → 收到通知 | 1304–1999 ms，中位数 1656 ms |

- 不同 API 时钟精度导致个别末段出现约 -1 ms 的差值，按近零理解。错开启动的进程出现几乎相同的通知时间，观察到批量交付。瓶颈位于 WMI 事件生成之前；尚未进一步拆分其内部收集/缓冲，不能据此声称某个具体内部定时器已确认。
- 证据 `.test-data/events-live-CWxUN7/evidence.json`，临时任务已清理，`cleanup.json` 的 `removeExit=0`。当前 WMI 的秒级延迟仍存在；本次没有替换事件源，也没有宣称提权带来即时通知。
- `npm run check` 通过；完整 `npm test` 38 项通过，0 失败/跳过，约 146 秒。自动回归模拟前台授权结果以避免弹 UAC；管理员通知链路及分段延迟由上述独立实机测试验证。

## 管理员事件监听

- 新增菜单“6 Guard → 3 授权管理员事件监听”及 `guard events enable|disable|status`。首次通过 UAC 安装固定监听脚本与最高权限任务，普通 Guard 只接收通知、按原权限处理应用。已有保护会在授权后刷新 Guard 和普通登录任务。
- 固定脚本放在 Program Files，目录、脚本和任务的所有者为 Administrators；用户只有读取/运行任务权限。注册使用 `TASK_DONT_ADD_PRINCIPAL_ACE`，避免系统自动追加可改写高权限任务的用户 ACE。监听端不执行客户端命令，断开连接即退出。
- 真实 UAC 测试（独立临时任务）验证：普通客户端调用任务成功、真实进程启动事件到达、客户端仍未提权、普通用户不能写入监听脚本、任务用户 ACE 仅为读/执行、监听退出后按需重启无需再次 UAC。
- 两个样本通知延迟分别 1995 ms、1051 ms；这是进程通知延迟，不是应用重新启动完成耗时，也不保证始终比扫描更快。证据 `.test-data/events-live-MlfmpL/evidence.json`。
- 临时任务及监听脚本已清理，清理结果为 `removeExit=0`；未重新配置现有用户 Guard 或目标应用。未执行重启电脑实测；登录恢复由原普通 Guard 登录任务和按需启动监听组成。
- 初次实测发现任务计划会把 Principal.UserId 规范化为账号名，以及 PowerShell 5.1 调用 File.Replace 的空备份路径转换问题；已分别按 SID 校验和显式临时备份修正，并通过后续安装/清理验证。
- `npm run check` 通过；完整 `npm test` 37 项通过，0 失败/跳过，约 145 秒。真实提权测试通过 `node tests/elevated-events-live.ts` 显式运行，不纳入普通测试以免自动弹 UAC。

## Guard 进程启动事件与扫描降级

- 使用独立 PowerShell/C# 辅助进程订阅 `Win32_ProcessStartTrace`，避免同步原生桥阻塞事件。启动扫描、事件唤醒、30 秒补漏共用原纠正流程；监听失败后退回 2 秒扫描，并每 30 秒尝试恢复。
- 去掉固定 1.2 秒启动宽限；缺失进程信息或空命令行只做最多三次短暂重试，不能按无代理实例直接终止。继续验证完整路径、PID、创建时间、当前用户/会话及分身目录。
- 新增测试验证事件唤醒、合并连续通知、串行检查、有限重试、监听失败后的扫描降级、退出时关闭监听，以及刚创建但参数可读的目标无需等待宽限。事件调度测试使用受控通知，不能据此声称真实 Windows 通知达到特定延迟。
- 本机未提升权限的真实 WMI 订阅返回 `AccessDenied`；辅助程序编译和权限失败反馈已实测，真实事件投递及加速幅度未验收。没有提升权限或更改 WMI 安全设置。
- 完整集成测试中真实后台 Guard 记录 `guard-events-fallback AccessDenied`，随后成功纠正隔离的受控程序；原有代理、分身、回滚及清理回归通过。没有对用户现有应用或 Guard 进行重新部署。
- `npm run check` 通过；完整 `npm test` 34 项通过，0 失败/跳过，约 142 秒。便携包包含新辅助程序与对应文档。

## 独立 sing-box 服务与程序复用

- 添加代理时先发现已有服务；读取标准 sing-box 进程及其 TCP 监听端口，核对 version、真实代理请求和进程身份后登记。启动复用入口不会安装或启动自建内核，不接管原服务配置。
- 新增真实独立 sing-box 集成测试：发现并复用其 HTTP 入口；复用同一个已有 EXE 启动另一个自有实例，生成配置只包含自己的端口；改名、清理内核及全部清理后，原服务存活、原配置逐字节保持一致、程序文件保留。普通 HTTP 服务器不能作为 sing-box 登记。
- 在不含随包内核的隔离目录验证缺少程序时的自动安装：从本机缓存的官方 ZIP 校验、解压、校验 EXE/DLL 并执行 version。本次未重复下载互联网制品，下载仍使用固定官方 URL 与 SHA256。
- `npm run check` 通过；完整 `npm test` 30 项全部通过，0 失败/跳过，约 131 秒。
- 当前用户 Guard 已刷新（PID 32308），已有内核 PID 3180 保持运行。没有切换现有应用代理，也没有接管其他用户服务。本次生成新的便携包，包含此前默认目录、锁、Guard 和启动性能修复。

## 整体审查四项修复

- 默认入口通过 `%USERPROFILE%\.app-proxy-home.json` 共用实际数据目录，显式 home 保持独立。本机无包身份进程（15700）执行不带 `--home` 的 `status`，正确读到 3 个应用、1 个代理，与快捷方式一致；证据 `.test-data/verify-default-home.json`。
- 模拟一个 MSIX 应用已卸载，另一个应用仍完成 Guard 纠正；错误只记录到失效登记。后台 Guard 已更新，PID 35244。
- 真实内核回归保留失效出口，验证正常应用冷启动、通用内核启动均成功；失败应用不会停止已运行内核。修改节点失败仍回滚旧配置。
- 目录/PID 锁改为 Windows 独占文件句柄。真实终止测试锁持有进程后，六个并发操作保持互斥；错误 token 不能释放其他请求持有的锁。
- `npm run check` 通过；完整 `npm test` 28 项通过，0 失败/跳过，约 120 秒。当前源码、文档与本机 Guard 已更新，release ZIP 未重新打包。

## 启动延迟优化

- 同机同一组 34 个相关进程的只读测量：原生进程枚举与归属核验从 9692 ms 降到 287 ms。原来逐个 WMI GetOwnerSid，改为 OpenProcessToken / GetTokenInformation 查询 SID，同时 GetProcessTimes 校验创建时间，保留用户、会话及 PID 复用保护。
- 已知 MSIX 包身份改为按包名称查询后精确校验包家族，避免每次遍历全部包。Guard 的查询与进程扫描移出共享锁，持锁后重新比较配置快照，期间配置改变则丢弃本轮扫描；初始化已有完整配置不再无条件获取写锁。
- 当前实机 Guard 一轮扫描约 903 ms，持锁部分仅约 3 ms；该采样未触发纠正，真正纠正仍需要持锁执行。
- 保留代理联网探测，没有把远端健康检查替换为仅检查端口。基线该探测 847 ms，后续样本随网络波动约 349–814 ms。
- 当前用户后台 Guard 已刷新为新代码（最终 PID 23668）。在包外进程实际执行 Claude 分身桌面 .lnk：观察到新主进程 3716 ms，首次观察到窗口 3972 ms，PID 34360，携带预期代理，已有原版进程未变。此为单次样本，包含应用自身启动和轮询观测耗时；没有测量优化前的完整窗口耗时，因此不宣称完整启动固定减少某个秒数。
- 并发回归首次出现 Windows rename EPERM，单独重跑未复现。配置原子替换增加对 EPERM/EACCES/EBUSY 的有限重试，最多累计等待 525 ms；仍保留旧文件，不通过删除目标文件规避错误。用真实 Windows 文件共享锁保持 250 ms 的测试验证了重试与完整写入。
- 最终 `npm run check` 通过；完整 `npm test` 25 项通过，0 失败/跳过，约 101 秒。新增进程身份不匹配时拒绝终止、Guard 扫描不占用启动锁且配置改变时丢弃快照、临时共享锁下原子写入回归。
- 证据：`.test-data/launch-timing-baseline.json`、`launch-timing-refreshed.json`、`launch-timing-final.json`、`outside-launch-timing.json`，实机脚本 `outside-launch-timing.ps1`。只修改当前源码与正在运行的 Guard，release ZIP 未重新打包。

## 桌面点击“应用不存在”的修复与包外启动验收（最新）

- 根因与图标一致：从 Codex 包内创建的应用登记实际位于包缓存目录，但快捷方式 `--home` 使用逻辑 AppData 路径，包外 CLI 因而读到另一份空配置。此前包内 Service 启动成功没有覆盖这个入口边界。
- Store 初始化改为从实际 manifest（新目录则从归属标记）文件句柄解析物理根目录，配置、锁、运行状态、快捷方式及 Guard 使用同一真实目录。`.store-scope.json` 保存原逻辑目录标识，保持已有 Claude LocalState 分身目录和 Guard 任务名不变；既有 Codex 分身旧逻辑参数仍能被准确识别，防止重复启动。
- 已重建三个桌面快捷方式及原 Guard 登录任务，`--home` 都指向现有真实数据目录。Guard 运行 PID 13652。没有搬迁、复制或删除已有应用数据，也没有删除包外的另一份空配置。
- 以一次性隐藏计划任务进入无包身份进程（GetCurrentPackageFullName=15700）读取三个真实 .lnk，三条 `--home` 均能找到对应登记，包外 CLI status 成功。
- 从该进程实际执行 Claude 原版、Claude 分身两个桌面 .lnk：分别启动 PID 46416、46200，均带预期代理参数，原版与分身网络子进程分别观察到 13、12 条内置代理连接。Claude 分身沿用原有 LocalState 路径。
- Codex 分身 PID 39424 和原版 PID 36424 保持运行；包外执行相同 home/appId 的 launch 命令正确拒绝重复启动，错误为“已经运行”，未出现“应用不存在”。没有为验证而关闭当前 Codex 会话。
- `npm run check` 通过；完整 `npm test` 22 项通过，0 失败/跳过，约 147 秒。新增回归覆盖虚拟目录映射、物理路径重新打开仍读取相同登记、分身命名空间和 Guard 名称稳定、旧运行实例识别。
- 实机证据：`.test-data/outside-launch.json`、`outside-launch-verify.json`，脚本 `outside-launch-smoke.ps1`；临时诊断任务均已撤销。源码与当前快捷方式已修复，release ZIP 未重新打包。

## 快捷方式原应用图标与更新路径

### 截图反馈后的根因确认与修复（最新）

- 用户截图确认三个桌面图标仍为空白；前次包内 SHGetFileInfo 成功并不能证明 Explorer 可读。最终发现逻辑路径 `Local\AppProxy\icons` 下的文件实际位于 `Local\Packages\OpenAI.Codex_2p2nqsd0c76g0\LocalCache\Local\AppProxy\icons`，是 MSIX 文件路径重定向造成包内外视图不一致，非仅图标缓存问题。
- 打开实际 ICO 文件句柄，以 GetFinalPathNameByHandle 获取真实磁盘路径后设置 IconLocation；不能只解析父目录，因为实测父目录句柄仍返回逻辑路径，文件句柄才返回重定向位置。保留多尺寸图标与内容摘要文件名。
- 通过当前用户的一次性隐藏计划任务启动不带包身份的只读诊断进程，GetCurrentPackageFullName 返回 15700（无包身份）。三个旧逻辑图标路径 Exists 均为 false；三个新物理路径 Exists 均为 true，包外 SHGetFileInfo 加载成功，诊断 PNG 目视显示正确图标。临时计划任务已删除；未重启 Explorer 或目标应用。
- 证据：`.test-data/outside-icons.json`、`outside-icon-<短ID>.png`、`icon-outside-probe.ps1`。验证针对图标引用，未在本次通过桌面快捷方式重新启动应用。之前所有从包内读取的结果不能扩大解释为包外启动验收。

### 空白图标反馈后的首次修正（未解决包外路径问题）

- 用户反馈桌面图标空白。此前仅确认 System.Drawing 能解码，未验收 Shell 显示，验证不充分；旧图标仅单帧 32x32、低色深，未通知桌面刷新。旧 ICO 在本次 LoadImage 检查也可加载，因而不能将空白唯一归因于文件损坏。
- 改用 `native/icons.cs` 以只读资源映射读取 EXE 的 RT_GROUP_ICON/RT_ICON，保留完整图像数据，不执行 EXE；输出多帧 ICO。缓存文件按图标内容摘要命名，更新快捷方式引用并调用 SHChangeNotify UPDATEITEM，避免复用旧图标缓存。
- 三个真实快捷方式已更新：Claude 图标 13 帧、96609 字节；Codex 图标 8 帧、107770 字节。通过 SHGetFileInfo 从各个 .lnk 取得 Shell 图标，均成功；导出诊断 PNG 并目视确认 Claude 橙色图标和 Codex 图标正常显示，带快捷方式箭头。未声称直接观察到 Explorer 桌面窗口。
- 诊断图在 `.test-data/shell-App Proxy - <短ID>.png`。没有清空系统图标缓存或重启 Explorer，也未重启应用。

### 前次验证（已由上述实现替换）

- 创建快捷方式时从当前应用 EXE 提取图标，MSIX 先重新查询包登记得到当前版本路径。ICO 缓存到工具数据目录 `icons/<应用ID>.ico`，快捷方式通过 IconLocation 引用固定路径。
- 已更新本机 Codex 分身、Claude 分身和 Claude 原版的三个快捷方式。通过 WScript.Shell 读取确认 IconLocation 指向缓存 ICO，三个文件均可由 System.Drawing.Icon 解码；Claude 两个图标摘要相同、与 Codex 不同。TargetPath 仍为 wscript.exe，Arguments 均携带对应应用 ID。
- `npm run check` 通过；MSIX discovery 定向回归 1 项通过，覆盖安装版本路径变化、默认/自定义工作目录处理与已分配数据目录稳定性。没有执行真实应用升级；完整回归的 21 项通过记录来自此前验证。
- 原应用更新不需要修改快捷方式中的版本路径，因为启动参数只有工具路径及登记 ID。普通 EXE 的路径变更和工具目录搬迁不在该自动解析范围内。

## Claude 2.2553.1.0 原版普通模式实测（最新）

- 使用同一已安装 MSIX 的普通应用模式（无 `instance`，Chromium 适配），登记为 `Claude · 原版代理`，ID `0d31d9e2-075b-421c-a681-75c4eae81166`，绑定现有内置代理 `127.0.0.1:18099`。
- 原版运行时再次通过工具启动，明确拒绝重复启动。按用户本次授权停止原版 PID 43568，再通过正式启动流程启动 PID 46008，主进程只有代理参数，没有 `--user-data-dir`；辅助进程仍使用原先的 `C:\Users\Admin\AppData\Roaming\Claude`。
- 原版网络服务 PID 40288 实际建立到代理入口的 TCP 连接。没有为此登记创建 user-data 或 claude-home；没有修改 Claude settings、复制或读取登录凭据。
- Guard 实测：停止代理原版后，通过相同 MSIX 通道不带任何参数启动 PID 35592；Guard 自动替换为带代理的 PID 44172，事件日志记录 `guard-corrected ... replaced=35592`。其进程树在采样时有 10 条已建立的代理入口连接，未观察到其他已建立 TCP 连接；此快照不证明全部协议或所有时段的流量行为。
- 已有分身 PID 6608 及创建时间始终未变，原版和分身同时运行。原版登记已启用 Guard，最终保持代理运行；桌面入口 `App Proxy - 0d31d9e2.lnk`。
- 证据保存在 `.test-data/claude-original-live-2026-09-18.json` 及 `claude-original-connections-before-guard.json`、`claude-original-connections-after-guard.json`；复现脚本为 `claude-original-smoke.ts`。本次未修改生产逻辑，无需重跑此前通过的 21 项自动化测试；增加的是实机验证与文档。
- 验证范围是原版目录沿用、进程启动、实际代理连接、重复启动拒绝和 Guard 纠正。未执行登录或模型对话，也未验证 Code/Cowork；不据数据目录相同就宣称登录功能已验收。

## Claude 2.2553.1.0 分身实机验证（前次）

- 当前用户安装的 MSIX 包：`Claude_2.2553.1.0_x64__pzs8sxrjxfjjc`，清单自动识别家族 `Claude_pzs8sxrjxfjjc`、应用 ID `Claude`，没有硬编码到启动逻辑。
- 首次实机启动发现 AppData 文件虚拟化问题：包内辅助进程无法读取工具的请求文件（ENOENT），无法生成回执。Codex 清单关闭该功能，Claude 未关闭。改为按 manifest 自动选择包 LocalState 内的工具专属目录，实测包内外请求/回执可见，启动成功。
- 分身 ID `8f570fb4-f1db-4d8f-9eaf-fe2b67eb7f14`，绑定现有内置内核入口 `127.0.0.1:18099`。分身 PID 37248 与原版 PID 43568 同时存在，各有窗口；分身独立 user-data 初始化 Cache、Network、IndexedDB 等数据。
- 分身网络服务 PID 45008 实际建立到 `127.0.0.1:18099` 的多条 TCP 连接。没有通过登录后的模型请求或 CDP 出口正文验收；该版本程序会拒绝远程调试参数，临时调试参数已移除，没有修改程序或其验证机制。
- Guard 实测：精确关闭测试分身后，以相同独立目录、不带代理重新启动 PID 39824；Guard 将其替换为带代理的 PID 6608，并写入 `guard-corrected`。原版 PID 43568 及创建时间保持不变。新 Guard PID 27112。
- 已创建桌面快捷方式 `App Proxy - 8f570fb4.lnk`；分身和 Guard 保持运行。工具只创建目录及设置进程环境，Claude 自行生成的 config 文件属于应用初始化，工具未修改 Claude settings 或复制登录数据。
- `npm run check` 通过；完整 `npm test` 21 项全部通过，0 失败、0 跳过，约 147 秒。新增 LocalState 路径隔离、不同工具 home 分离、包更新后目录稳定性、两种分身在两种存储模式下的启动回执/过期/防重试回归。
- 证据：`.test-data/claude-live-2026-09-18.json`；读取已安装 app.asar 中启动参数校验实现仅用于定位调试参数退出原因，没有修改安装文件。源码已更新，release ZIP 未重新打包。

## 0.2 Claude 分身（前次受控验证）

- 加入 `instance: claude`，中文菜单可选普通应用 / Codex / Claude 分身。Claude 使用独立 `user-data` 和 `claude-home`，通过 `--user-data-dir` 与子进程 `CLAUDE_CONFIG_DIR` 传入；不处理 settings 文件，不复制登录状态。
- Codex 继续使用原有目录和变量。启动分身前按大小写不敏感规则清除继承的 Codex/Claude 目录变量，只设置对应类型的值；MSIX 白名单补充 CLAUDE_CONFIG_DIR。
- `npm run check` 通过；完整 `npm test` 19 项全部通过，0 跳过，约 136 秒。CLI help 已检查。
- 真实受控程序验证：同一 EXE 的原实例、两个 Claude 类型分身、一个 Codex 类型分身同时运行；各自目录和代理请求正确；Claude Guard 仅纠正所选分身，其他三个实例保持存活。启动器创建的应用 home 目录为空，没有写入 settings 或凭据。
- 两种分身分别通过普通实例/辅助进程归属、冲突参数拒绝、MSIX 环境与参数回执、过期请求拒绝和防重复启动测试。
- 本节最初使用受控程序验收；安装真实 Claude 后的结果见下方“Claude 2.2553.1.0 实机验证”。登录、模型对话、Code/Cowork 仍未验收。
- 当前源码已更新，现有 release ZIP 尚未重新打包。

## 0.2 移除外部代理类型与 MSIX 通用性检查（前次验证）

- 删除 external 类型、旧命令、兼容提示、服务/启动/Guard 分支和外部监听端口发现接口。Profile 只接受 managed 类型，节点列表必填，旧的不支持类型在读取或保存时直接校验失败。
- `npm run check` 通过；完整 `npm test` 共 16 项通过，0 失败、0 跳过，约 116 秒；PowerShell 原生桥接语法检查通过。
- 新增配置校验测试：拒绝旧类型、未知类型、缺失类型、非回环入口和缺失节点。
- 原“旧外部代理拒绝启动”集成场景改为真实内置内核上游不可用：启动失败后未创建应用进程，失败内核已停止，仍被应用引用的 Profile 不能删除。
- 新增非 Codex 包名的 MSIX 解析测试：自动保存包身份、根据包身份刷新 EXE/默认工作目录、保留自定义工作目录、拒绝未登记或非 full-trust 应用、普通 EXE 不进入包查询。这是受控包登记数据测试，不代表已经真实验收第二款 MSIX 应用。
- 只读校验本机现有 manifest：1 个 managed Profile，满足新校验规则；未迁移或删除用户配置。

## 0.2 自动网卡探测与内置内核（前次验证）

- `npm run check` 通过；`npm test` 共 14 项全部通过，0 跳过，约 99 秒。
- 内核来自原官方 ZIP，归档 SHA256 与固定值一致。随包携带 EXE、原版 libcronet.dll、LICENSE 和 SOURCE.txt；启动只使用 `runtime/sing-box/sing-box.exe`，EXE/DLL 均校验 SHA256。
- 新增网卡筛选测试：优先 Wi-Fi、再按路由与接口 metric 排序，排除 TUN/Wintun、虚拟、断开、无默认路由及仅链路本地地址的网卡；中文和空格别名正确写入 JSON。
- 新增真实内核故障测试：模拟已消失的候选网卡，真实非回环地址上的受控 HTTP 上游使绑定失败；随后切换 sing-box 自动路由成功，状态保留回退原因。重启重新枚举，旧配置中的任意 EXE 路径不会被采用。
- 初始故障测试使用回环上游时未触发绑定失败，因而改用本机物理网卡地址验证；不把回环转发成功当作物理接口绑定证据。
- 原有真实 HTTP/HTTPS、多入口、受控应用参数/环境、Guard 后台纠正、Codex 实例隔离、MSIX 辅助程序、配置回滚、任务/快捷方式与卸载回归全部通过。
- 当前用户配置已由旧 `.tools` 路径切换至内置程序，PID 3180；自动选择物理网卡“以太网”，没有回退。真实订阅节点 HTTPS 健康探测 HTTP 204，出口 `64.118.152.127`；核心进程到节点及 DoH 的已建立 TCP 连接使用该物理网卡地址。
- Guard 已以新版代码重新启动，PID 37832；原 Codex PID 36424 和分身 PID 39424 保持存活，没有重启目标应用。
- 本机现有 TAP/Wintun 网卡均未连接，因此没有执行“另一个 TUN 正在接管系统流量”的共存验收；没有更改系统路由、启用 VPN 或宣称可绕过 WFP 强制拦截。网络切换后需要重启内核重新探测。

下方内容为此前版本的验证历史；外部端口复用、用户选择内核等旧功能已从当前使用流程移除。

## 环境

- Windows 25H2，build 26200.9168，x64。
- Node.js 24.17.0；TypeScript 5.9.3（仅开发检查）。
- Windows 系统 PowerShell 5.1 辅助进程；调用工具的终端为 PowerShell 7.6.5。
- 真实 sing-box 1.14.1 Windows amd64；官方 ZIP SHA256：`5197f16d492d93202dc623622149a6ed040f8eca263128f91d603f2b901baa89`。

## 已执行

`npm run check` 通过。Windows 端到端测试覆盖多个行为，最新完整测试数量见下方补充记录。

| 行为 | 实际证据 |
|---|---|
| 六类订阅节点 | AnyTLS/VLESS/VMess/SS/Trojan/Hysteria2 解析成功，并由真实 sing-box check 接受生成配置 |
| 订阅容器 | 普通 URI、Base64、Clash 行内/嵌套节点、Surge/Loon/Shadowrocket 风格及 Quantumult X 配置样本解析通过 |
| 订阅刷新 | 保留选择，报告移除；重名或清空已选出口时拒绝更新 |
| 两个不同入口 | 两个本地 sing-box HTTP 入口分别命中 A/B 两个受控 HTTP 上游代理，来源有独立上游事件记录 |
| HTTP/HTTPS | 真正返回受控服务响应；HTTPS 经 sing-box 与上游 CONNECT 隧道，客户端只信任测试证书，无中间人解密 |
| TLS 错误 | 未信任的测试证书被拒绝 |
| 非代理端口 | 普通 HTTP 服务不能通过 HTTPS CONNECT 探测 |
| 应用启动 | 从带中文和空格的 EXE/工作目录启动；中文、空参数、引号、尾部反斜杠完整到达子进程 |
| 子进程环境 | 获得预期代理变量，父进程环境未改变；直连模式清除代理变量 |
| 应用实际请求 | 受控子程序读取注入的代理地址，通过真实 sing-box 请求受控 HTTP 端点并得到期望正文 |
| 已运行/代理故障 | 拒绝重复启动；代理端口未监听时未创建新的受控程序 |
| 配置失败 | 非法节点不提交；语法合法但死上游导致更新失败后，恢复旧配置，旧入口再次成功转发 |
| Guard 单次纠正 | 误启动受控程序被关闭并由正确代理参数重新拉起，已正确代理的实例保持运行 |
| Guard 后台进程 | 实际运行后台 CLI，纠正第二次误启动；重复启动复用原后台进程，禁用后退出 |
| 进程归属 | 故意传入错误创建时间，停止操作被拒绝，目标进程仍在 |
| Windows 集成 | 专属测试快捷方式创建/撤销；当前用户任务注册、重复注册和撤销 |
| 卸载 | 连续两次全部清理可完成；原应用 EXE、用户提供的 sing-box、外部上游和配置备份保留 |

另已通过真实 CLI 执行 `core install`，下载官方制品、核验摘要、从 ZIP 提取程序并读取版本；`discover` 正确列出当时测试内核的两个监听端口为未验证候选。

中文菜单已在 Windows 终端打开，完成查看状态和返回菜单的交互。测试中的快捷方式、计划任务和进程均使用测试目录及唯一 ID，不操作现有用户应用。

## 验证边界

- 六类协议的真实远端服务连通性没有全部验证；受控 HTTP 上游和真实 AnyTLS 美国节点已验证。
- 已使用用户指定的真实订阅；未验证每种客户端配置的全部扩展字段。
- Claude 和 Codex 本机版本的后续实机验证见对应补充章节；登录后的模型对话和实际升级尚未验证。
- 登录任务的注册/撤销已验证；未通过登出再登录验证触发。
- 代理请求和启动证据不代表全流量隔离；Guard 仍有轮询窗口。
- 当前交付为 Windows x64，尚未验收 ARM64 或组织策略禁用 PowerShell/WSH 的环境。

源码测试入口：`tests/config.test.ts`、`tests/subscription.test.ts`、`tests/integration.test.ts`。

## 2026-09-18 真实订阅与分身补充

- 本轮类型检查通过，完整测试运行 8 项通过（端到端约 54 秒）；随后新增的实例/辅助进程范围测试单独执行通过，共 9 项通过，无跳过。受控测试进程已退出。
- 真实订阅：修复原版 YAML 列表层级问题，85 个 AnyTLS 节点、0 个不支持项；85 个节点的 ALPN 数组正确保留。刷新保留选择，新增/移除均为 0。
- 托管入口：`127.0.0.1:18099`，美国 A 服务器 01，HTTPS 健康端点返回 204，出口探测返回 `64.118.152.127`。这是工具发起的请求，不是 Codex 流量证据。
- 指定 DoH：直接发送 DNS wire-format POST，HTTP 200、application/dns-message、事务 ID 匹配、RCODE 0、2 个答案；生成配置已设置该 DoH。
- 受控多实例：同一测试 EXE 的普通实例及两个分身同时存活，各自收到独立 CODEX_HOME/user-data 和对应代理；两个分身实际发出受控代理请求。
- Guard：纠正分身 A 的无代理误启动时，普通实例和分身 B 保持存活；重复启动同一分身被拒绝。
- 本机 Codex：安装包 `OpenAI.Codex_26.911.7940.0_x64__2p2nqsd0c76g0`，实际主程序为 ChatGPT.exe。只读检查 app.asar 确认 CODEX_ELECTRON_USER_DATA_PATH 与 CODEX_HOME 的启动处理。
- 实际双开未通过：Node spawn 返回 EPERM；随后通过 Windows ProcessStartInfo 获取更具体启动结果的命令被自动审批拒绝（blocked by policy，未提供具体理由）。没有采用其他启动方式绕过拒绝，也未关闭现有 PID 29344。
- 真实订阅及节点凭据只存于当前用户 ACL 保护的 `%LOCALAPPDATA%\AppProxy`，不进入源码、测试样例和便携包。

## EPERM 只读定位补充

此节记录适配前的诊断。直接启动失败存在独立于工具自动审批的 Windows 权限原因：旧启动器通过 Node spawn 直接执行 MSIX 包内 ChatGPT.exe，没有包上下文适配。后续修复及验收见下一节。

- 安装包状态正常；AppId 为 App，EntryPoint 为 Windows.FullTrustApplication，未声明 ExecutionAlias。
- 包内 EXE 的 DACL 给普通 Users 读取权限；读取/执行授权包含 `WIN://SYSAPPID Contains "OpenAI.Codex_2p2nqsd0c76g0"` 的包身份条件。
- 仅打开并关闭文件句柄的权限探测：读取 ChatGPT.exe 成功（0），申请执行访问失败（5 / ERROR_ACCESS_DENIED）；对系统 cmd.exe 的执行访问检查成功（0）。没有调用进程启动、没有修改 ACL。
- 诊断进程 GetCurrentPackageFullName 返回 15700（APPMODEL_ERROR_NO_PACKAGE）。libuv 会把 Windows ERROR_ACCESS_DENIED 映射为 UV_EPERM，与实际 spawn EPERM 相符。
- 因此，先前主要归因于自动审批的判断不完整：自动审批确实拒绝过后续启动诊断；现有启动器本身还没有适配此 MSIX 包的启动权限模型。两者需分别处理。
- 后续方向是评估 Windows 包激活接口及其参数/环境传递能力，不能据此宣布 Codex 双开已可用。原实例保持运行，未更改安装目录、签名或权限。

参考：[libuv Windows 错误映射](https://github.com/libuv/libuv/blob/v1.x/src/win/error.c)、[GetCurrentPackageFullName](https://learn.microsoft.com/en-us/windows/win32/api/appmodel/nf-appmodel-getcurrentpackagefullname)。

## MSIX 适配后的实机验收

2026-09-18，Windows 当前用户、非管理员上下文；Codex 包 26.911.7940.0。

- 生产实现：Get-AppxPackage/Manifest 解析包身份，Invoke-CommandInDesktopPackage 启动 Node 辅助程序，辅助程序设置代理和分身环境后启动真实 EXE。未使用私有 COM 接口，未修改安装文件、权限、签名、用户全局环境或原应用配置。
- 普通 Shell AppsFolder 激活能够创建第二个进程，但不会调用环境回调；实测独立 codex-home 仍为空，因此已移除该实验实现，未作为完成结果。
- 包上下文启动后，独立 codex-home 出现 config.toml、state_5.sqlite、goals_1.sqlite、logs_2.sqlite 等初始化文件；界面 user-data 目录也独立生成。未读取或复制原用户认证文件。
- 集成启动器真实返回分身 PID 38236；原实例 PID 36424 保持运行。运行态保存目标创建时间、路径和所属用户，重复启动仍拒绝复用已有实例。
- 通过仅在测试分身开启的 CDP，在该浏览器上下文创建出口诊断页面并重新加载：HTTPS api.ipify.org 返回 HTTP 200、页面 IP 为 64.118.152.127；Network.responseReceived 的 remoteIPAddress=127.0.0.1、remotePort=18099。诊断页面随后关闭。
- 主界面直接执行跨域 fetch 未成功，因此没有把该失败请求作为流量成功证据；上条证据来自独立诊断页面的实际导航。
- 实际 Guard 后台验证：创建同一独立目录、缺少代理参数的 Codex PID 33020，Guard 关闭它并通过生产 MSIX 启动器拉起 PID 39424。校验预期代理参数，原 PID 36424 的身份未变；最终分身无 remote-debugging-port 参数。Guard PID 15528，仅保护本工具分身。
- `npm run check` 通过；`npm test` 完整 10 项通过，无跳过。新增测试覆盖包上下文辅助程序的参数/环境交接、过期请求拒绝和未确认启动禁止重复提交。
- 边界：没有登录后发起模型对话；本次出口证据覆盖真实 Codex 浏览器请求，不证明后台所有网络库或任意子进程的所有流量。未实际升级 Codex/Windows 后再次验收；接口定位和支持范围见 README。
