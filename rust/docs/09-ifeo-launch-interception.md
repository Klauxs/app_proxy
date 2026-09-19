**IFEO 启动前接管**

补充日期：2026-09-19，实施状态更新：2026-09-20。用户确认的需求是：通过 IFEO Debugger 拦截原始 EXE 启动，再交给启动器注入实例与代理配置。已有调试创建/脱离候选实验及注册/恢复平台层；入口和完整接管尚未实现。本文是目标设计，不是应用级兼容性结论。

**1. 在整体架构中的位置**

IFEO 是 Guard 的启动前接管组件，也是 LaunchEngine 的新增入口。菜单、快捷方式和 IFEO 共用代理准备、实例预留、环境构造、进程确认与恢复；ETW 是 Guard 的启动后身份检查和纠正组件。

```text
用户从已验证的原始入口启动应用
  → Windows IFEO Debugger 重定向
  → app-proxy-host.exe ifeo-entry --registration <id> <原始命令行>
  → 校验安装记录、目标、令牌、session 和调用类型
  → coordinator 接收 InterceptRequest
  → 解析默认实例并进入 LaunchEngine
  → 验证代理、准备目录及环境
  → WindowsPlatform 使用已验收的防递归创建路径
  → 确认真实目标进程；返回回执
```

这条路径的目标是让命中的主进程在带好配置后才开始运行，避免先启动再由 Guard 关闭的窗口。但它不拦截网络，也不能证明应用的所有组件都遵循代理。没有命中规则、已有进程接收激活、其他启动机制或其他用户的调用不能算作已覆盖。

用户已确定：Codex/Claude 代理预设默认开启 Guard，已登记原版的 Guard 包含 IFEO，不要求用户单独打开 IFEO 开关。只创建分身时，原版未管理，不安装该 EXE 的 IFEO，也不要求创建原版配置。IFEO 按 EXE 路径匹配，无法用路径规则区分同一程序的原版/分身，因此首版用这条简单边界保证不干预原版。分身仍由专用启动流程和精确实例检查保护。

Guard 的前台启用流程完成适用组件的系统授权和安装。原版所需 IFEO 未就绪时显示 degraded/blocked；只管理分身时 IFEO 是 not_applicable，不属于保护故障。默认开启不等于已通过安装和兼容验收。

**2. Windows 机制与兼容边界**

微软说明，IFEO Debugger 会被加到原始命令行前面，调用方获得的进程信息属于 debugger 入口。因此原调用方可能观察到的是 host，而不是真实应用；不能把它包装成完全透明的 CreateProcess 替代。[微软机制说明](https://devblogs.microsoft.com/oldnewthing/20070702-00/?p=26193)。

仅验收明确列出的桌面入口。父进程依赖 PID、进程句柄、退出码、继承句柄、标准输入输出、Job 或特定 STARTUPINFO 的场景必须单独验证；等待 host 到应用退出也不能等价还原全部语义。无法保持必要语义时，该应用或该入口不开放 IFEO。

IFEO 注册位于机器级 `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options`。设计采用精确路径过滤候选 `UseFilter` / `FilterFullPath`，避免只按 `ChatGPT.exe` 等文件名接管所有安装；这些过滤值的匹配和注册表视图行为必须在 M0 验证，不视为已获得通用兼容保证。

机器级注册不是用户级设置。即使入口再检查 SID/session，其他用户对同一路径的启动也可能已经被重定向。首版管理路径只接受安装时授权的用户、普通完整性令牌和对应 store session；不支持的调用拒绝并返回明确原因，不静默直连，也不转交其他用户的 coordinator。启用摘要必须说明这一影响；需要共享使用该路径或提升启动的应用不开放首版 IFEO。跨用户/高完整性安全放行后端不在首版承诺中。

**3. 注册与实例模型**

IFEO 按安装路径匹配，不能替每个分身注册一条相同 EXE 规则。新增 `IfeoRegistration`，持久化在 `manifest.integrations.ifeo`：

| 字段 | 含义 |
|---|---|
| id、revision | 注册身份和配置版本 |
| application_id、default_instance_id | 目标安装及其已登记原版；不允许指向分身 |
| desired | enabled / disabled，由受管原版 Guard 驱动，不是独立用户开关 |
| owner_sid、store_id | 管理归属；不代表注册表天然按用户隔离 |
| installed_target | 已授权的完整 EXE 路径、文件身份、可选包版本/身份 |
| registration_generation | 与受保护安装清单及系统注册回读相对应的版本 |

实际状态单独记录为 disabled、needs_authorization、active、stale、conflict、unsupported 或 blocked，不以 desired enabled 代表已接管。受保护的安装清单保存注册表视图、子项、原值、本产品写入值、host 位置/版本及恢复信息；普通 manifest 不是提权操作的唯一授权依据。

同一物理 EXE 路径只允许一个本产品 IFEO 所有者，跨 store 冲突也拒绝。只有同一 Application 的原版已登记、绑定代理且启用 Guard，才创建 IFEO 注册，原始入口固定路由这份受管原版。只有分身时没有此注册，未管理原版正常启动，不自动创建原版记录、套用分身代理、提示补齐原版路由或增加默认实例重绑定选项。

专用快捷方式仍直接携带 instance ID。存在本产品 IFEO 规则时，内部启动在系统创建前选择防递归路径，不重新经过原版路由；没有规则则用普通创建。关闭原版 Guard 或删除原版前必须解除对应 IFEO 注册；解除失败时报告操作未完成，不把原版标成已解除管理。其余分身可以保留并继续运行。

**4. 入口解析与统一状态机**

`ifeo-entry` 只是受限入口适配器，不接受任意 `--home`、执行器路径或脚本。registration ID 定位已安装清单，再校验固定目标、当前令牌和实际路径；注册 ID、命令行标记或环境变量均不构成身份凭证。

入口保留 Windows 原始宽字符命令行，按已验证的 Windows/模板规则解析，不使用空格 split，也不经 cmd/PowerShell 重新执行。启动器自己的固定参数与 Windows 追加的目标命令行有明确解析边界；空格、引号、尾反斜杠、中文和异常 argv[0] 均有 fixture。

2026-09-20 已实现只读入口准备 API：`capture` 用 GetCommandLineW 读取原始宽字符串，仅接受已生成的带引号 host、固定模式、规范 UUID 和 ` -- ` 边界；目标后缀原样保留，再用 CommandLineToArgvW 解析为 OsString，不经有损 UTF-8 转换。只有核验后的完整登记能提供 store/instance 路由，不接受外部 `--home`。`verify` 要求普通交互会话的 medium token，拒绝 restricted、UIAccess、AppContainer，再核对 protected registry/deployment、实际 host 路径/fileID 和目标路径；目标 pin 只打开受保护登记中的路径。返回对象继续持有 deployment 和目标句柄，并提供派发前复核。该对象不证明请求来自内核 IFEO，也不是启动或辅助 continuation 授权；同用户手动构造入口仍需经过后续服务端配置和调用类型核验。

此层的 5 项测试覆盖原生参数解析、UTF-16 保留、异常边界、身份策略以及当前普通 token/原始命令行读取，未执行真实 IFEO 启动。实际 host 模式、coordinator 转交、cwd/环境及 STARTUPINFO/Job/继承句柄语义仍待接入和验收。[GetCommandLineW](https://learn.microsoft.com/en-us/windows/win32/api/processenv/nf-processenv-getcommandlinew)、[CommandLineToArgvW](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-commandlinetoargvw)。

`InterceptRequest` 包含 request_id、registration_id/generation、已校验的目标身份、调用类型、允许转发的参数、cwd 与环境快照；令牌/session 从系统连接身份核验。原始 URL、文件名和环境可能含秘密，不写诊断日志。环境遵循第二章的过滤和大小限制。

主进程请求复用 Accepted → Confirmed/Failed/Indeterminate 状态机，记录 `LaunchOrigin = interactive | shortcut | guard | ifeo`。入口重连查询同一个 request_id；同一物理实例已有 pending 时返回原 attempt。代理不可用、配置冲突或 coordinator 不可达时不创建目标进程。

只转发模板明确支持的文件/协议激活参数；外部参数不能覆盖代理、实例目录等受管字段。未知参数组合拒绝，不能为了“兼容原启动”直接拼接全部输入。已有同配置进程时，无载荷调用可返回 AlreadyRunning；携带文件/URL 的调用需要模板已验收的激活转发，否则返回 `IFEO_ACTIVATION_UNSUPPORTED`，不能丢弃载荷却报告成功。

**5. 防止递归触发：平台后端的前置条件**

在 IFEO 规则仍生效时普通 spawn 同一 EXE 可能再次启动 ifeo-entry，因此不能把“带参数再运行原 EXE”当成完整实现。设置 `APP_PROXY_BYPASS=1` 也不会让 Windows 忽略注册规则。

M0 研究调试创建及脱离方案，候选使用 `CreateProcessW` 的调试标志，以专用线程消费调试事件、核验目标并完成脱离；具体标志组合及其跳过 IFEO 的行为必须实测后锁定。官方定义了 `DEBUG_PROCESS` 和 `DEBUG_ONLY_THIS_PROCESS` 的调试范围，但这不等于本方案已获 IFEO/MSIX 兼容保证。[创建标志](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags)。

不能设置调试标志后立即退出而不处理生命周期。候选后端需在建立调试关系后处理 `DebugSetProcessKillOnExit(FALSE)`、事件继续和 `DebugActiveProcessStop`，明确句柄释放及创建线程职责。默认调试器退出可能终止目标；创建成功到调整退出行为之间的崩溃窗口也要故障注入。相关调用失败、脱离失败或回执丢失时进入受控清理或 Indeterminate，不能误报成功。[退出行为](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-debugsetprocesskillonexit)、[停止调试](https://learn.microsoft.com/en-us/windows/win32/api/debugapi/nf-debugapi-debugactiveprocessstop)。

禁止“临时删除 Debugger → 启动 → 写回”：该方案在机器范围产生并发漏拦截窗口，崩溃后也可能留下错误系统状态。禁止通过改名/复制目标 EXE 绕过规则；安装内容与包身份保持不变。

WindowsPlatform 对上层仍提供一个 spawn 契约，内部根据已安装规则选择普通创建或经验证的防递归机制。所有内部来源都必须走它，包括 CLI、快捷方式、Guard 和 package-child；不能只给 IFEO 发起的请求加例外。

**6. 同名子进程与 MSIX**

Electron/Chromium 常用同一个 EXE 创建 renderer、GPU、utility 等子进程，路径过滤不能区分这些角色。把每次命中都送进“启动默认实例”会破坏整个应用。模板必须区分外部主进程、已验证运行实例的辅助进程和未知调用。

辅助进程只允许在进程祖先、创建时间、用户/session、目标映像及实例归属均可确认时进入平台级 continuation，不建立新的 Instance/LaunchAttempt，不更改继承的网络/目录语义。`--type=renderer` 等参数或一个 bypass 标记本身不能授权放行。continuation 的父子关系、继承句柄、沙箱令牌、Job 和父进程等待语义必须实测；无法保持时，该应用整体不开放 IFEO，不能只声称主进程测试通过。

辅助 continuation 有独立关联 ID、回执及有界恢复记录，不受主进程“一个实例一个 attempt”锁阻塞；结果未知禁止重复创建。ETW 可能先观察到 ifeo-entry 或处于调试创建阶段的目标，Guard 必须识别这些内部状态，避免二次关闭/重启。

MSIX 继续按 family/AppId 作为稳定应用身份，但 IFEO 过滤绑定本次解析出的实际 EXE 路径。更新到新版本目录后状态变为 stale，旧规则不等于新路径受保护；ETW Guard 可继续按自身覆盖能力工作，界面明确显示 IFEO 未覆盖新版本。

更新过滤路径需要重新解析、前台授权并按安装 journal 切换，不能使用裸文件名扩大接管范围。包内 helper 的最终 spawn 也必须使用防递归后端，并验证包身份、虚拟化目录和激活行为。开始菜单、协议链接、应用自重启、已有实例激活分别验收；当前 PowerShell 包启动经验不能直接证明 IFEO 适配成立。

**7. 安装、恢复与卸载**

注册修改由固定提权安装维护模式执行；运行入口和决策仍使用普通权限。Debugger 指向管理员保护目录中的固定 host 副本，命令仅含固定模式及 registration ID，不指向便携目录里的可写 EXE，不存代理凭据。安装器校验目标、用户授权、文件身份、既有 Debugger/过滤项和产品归属；已有第三方规则默认返回 conflict，不覆盖或串联未知 debugger。

安装步骤：生成具体影响预览 → 前台授权 → 获取目标的机器级安装锁并回读冲突 → 写恢复 journal/备份原值 → 部署并验证 host 和受保护清单 → 写本产品过滤项 → 回读及受控验证 → 提交 generation。先在夹具完成基本验证，正式注册不自动关闭真实用户应用；未完成应用级实测的状态保持 blocked。

修改共享的 UseFilter 等父级值必须记录原始类型和值，验证对已有项的影响。失败只回滚仍等于本次写入内容的值；发现并发第三方改动时标记 conflict，不覆盖。清理只删除本产品拥有的值/子项，不删除整个 EXE 的 IFEO 键或其他 mitigation 设置。

卸载顺序为先解除本产品注册并回读确认，再删除受保护入口。host 缺失、目录移动、版本不匹配或 store 损坏时，应由独立可运行的 `integration repair` / `ifeo disable` 前台流程恢复；其提权侧读取受保护安装记录，不要求 coordinator 成功启动。不能依靠已经损坏的 IFEO 入口自救。

停用失败时保留 host 与恢复材料，并明确未解除接管。恢复旧值只在本产品当前值仍匹配且没有新所有者时执行。注册表、副本文件与 manifest 不具备全局事务性，各断点都必须可核对恢复。

2026-09-20 平台实施补充：受保护记录位于 64 位视图 `HKLM\SOFTWARE\AppProxyRust\Ifeo`，`Registrations` 子键以 UUID 命名单个 REG_BINARY 保存 Installing/Active/Removing/Removed 状态；单值发布消除“建了空记录子项但未写入内容”的断点。父值备份绑定 EXE 名称和格式版本，记录原始 UseFilter 的类型和值。规则本身仍位于系统 IFEO 树的精确路径过滤项。只接受 UseFilter 缺失、DWORD 0 或 1；第三方 Debugger、未知值和冲突过滤项保留并报告。

安装在持久 intent 后写 Owner/FilterFullPath，再启用 UseFilter，最后写 Debugger；解除先保存 Removing，再禁用自身 Debugger、删除仍匹配的自有过滤项，最后按共享参与者恢复父值并保存 Removed。管理员保护的全局 mutex 串行化本产品操作，不能使外部管理员写入成为全局事务；每步前后仍须核对，冲突不覆盖。逐级打开注册表项拒绝链接，受保护记录与 mutex 核对 owner/DACL。只读系统 ACL 核验接受 Windows 的 CREATOR_OWNER 继承占位，但实际 owner 仍须是管理员、SYSTEM 或 TrustedInstaller。[CREATOR_OWNER 说明](https://learn.microsoft.com/en-us/windows/win32/secauthz/well-known-sids)。

本机注册表事务 API 实测返回 6801，因此生产实现不依赖 TxR。15 项专项测试在随机独占 HKCU 子树进行真实写入，并只读检查 HKLM ACL；覆盖 7 个安装、5 个解除持久断点及首次记录发布前中断。这些结果不替代真实 HKLM 过滤匹配、提权 mutex/ACL 或启动语义验收。已完成记录暂保留，最多 512 条；到达上限拒绝新增但允许已有规则核验/解除，后续维护需要处理历史记录整理。当前没有生产 CLI 启用规则，必须先接通并验收启动入口和 continuation。

**8. 用户界面与诊断契约**

用户通过 Guard 完成启停，IFEO 状态与修复属于组件诊断。保留以下底层维护命令草案：

```text
app-proxy ifeo status [<application-id>] --json
app-proxy ifeo disable <application-id>
```

原版 Guard 启用预览显示实际 EXE、受管原版实例、代理、机器级影响和已验收入口；非交互缺授权返回 requires_action，不后台弹 UAC。关闭原版 Guard 必须解除其 IFEO 注册；解除失败时显示停用未完成，保留可恢复入口。底层 ifeo disable 用于故障恢复，不能让需要 IFEO 的原版 Guard 继续显示完整 active。

Guard 开关保持实例粒度，不再引入应用保护总开关。IFEO 是受管原版 Guard 的内部组件：关闭分身 Guard 不影响原版注册；关闭原版 Guard 则解除注册，分身不要求保留它。原版受管时，所有经过同一路径的内部创建仍需识别 IFEO 并避免递归。

doctor 展示注册 desired/actual、目标是否更新、host 是否有效、规则归属、防递归后端能力、最近 attempt 和 Guard 状态。错误至少包含 `IFEO_CONFLICT`、`IFEO_TARGET_CHANGED`、`IFEO_CONTEXT_UNSUPPORTED`、`IFEO_REENTRY_BLOCKED`、`IFEO_ACTIVATION_UNSUPPORTED`、`IFEO_BACKEND_UNSUPPORTED`，映射现有权限、配置、依赖和未决退出码。日志不输出完整原始命令行。

**9. M0 与发布门槛**

| 实验 | 必须获得的证据 |
|---|---|
| 注册匹配 | 相同文件名不同路径不误接管；验证注册表视图、过滤语义及第三方规则冲突 |
| 防递归 | 外部入口、CLI、Guard、package-child 都只创建一个目标；并发时没有临时卸载规则 |
| 管理范围 | 只有分身时没有 IFEO 注册，未管理原版及其子进程不被 Guard 关闭或改配置；移除原版保护后分身仍可正常启动 |
| 调试生命周期 | 调试事件、脱离、helper 崩溃和取消不会留下未记录进程/悬挂进程或错误成功状态 |
| 参数与激活 | 中文/空格/引号正确；明确处理文件/URL、已有实例激活和调用方等待行为 |
| 同 EXE 子进程 | renderer/GPU/utility、沙箱、句柄/Job 行为正常；伪造角色不能绕过实例校验 |
| 权限与共享路径 | 普通用户成功；高完整性、不同 SID/session 明确拒绝；启用资格与影响说明一致 |
| MSIX | 原版/分身、包身份、数据目录、开始菜单/协议入口、重启和包更新逐项记录 |
| 安装恢复 | 每个注册/文件提交断点可恢复；第三方新改动保留；入口丢失仍可独立解除注册 |
| 代理与 Guard | 代理失败时主进程尚未创建；ETW 不重复纠正内部创建或辅助 continuation |

先在隔离测试机/虚拟机中用受控 EXE 和测试包验证，再测试指定版本 Codex/Claude。未通过某应用的同名子进程或包激活测试时，该应用不能标记 IFEO active。M0 已运行不含注册表改动的调试创建/脱离实验，见 [验证记录](D:/app_proxy/rust/TEST-RESULTS.md)；尚未修改本机 IFEO 注册表或安装接管入口。
