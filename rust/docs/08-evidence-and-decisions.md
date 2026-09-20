**证据、决定与待验证事项**

本设计最初依据 Windows 源码、上一次开源调研中的固定上游提交，以及 Rust/微软官方资料编写。2026-09-19 已开始 M0 Rust 实现；实际构建与包内 helper 证据见 [验证记录](D:/app_proxy/rust/TEST-RESULTS.md)，其余流程仍是计划。现有 Windows/TypeScript 实现未修改。

2026-09-20 用户明确取消 IFEO，取代 2026-09-19 的加入需求及此前保留方向。实现移除注册/恢复、入口解析及调试脱离候选；Guard 保留 ETW 启动后检查。撤销决定见 [第九章](D:/app_proxy/rust/docs/09-ifeo-launch-interception.md)。历史验证记录保留历史含义。

**1. 当前基线**

开始读取时 HEAD 为 `a38a29865578035ba8ad0d85d0e319bbb63880e2`，分支 main。此前已有 ETW 监听和 Codex/Claude 桌面预设两项更新，因此不沿用早期“只有轮询/必须手填 EXE”的描述。

| 证据 | 用途 |
|---|---|
| [types.ts](D:/app_proxy/windows/src/types.ts) | 识别现有职责耦合，作为新对象拆分的参考 |
| [applications.ts](D:/app_proxy/windows/src/applications.ts) | 自动桌面识别、默认 Guard、参数/环境、实例进程匹配、launch |
| [cli.ts](D:/app_proxy/windows/src/cli.ts) / [service.ts](D:/app_proxy/windows/src/service.ts) | 最新添加流程、代理验证、默认保护与诊断范围 |
| [guard.ts](D:/app_proxy/windows/src/guard.ts) | 先关闭后 launch、旧绑定宽容、限流及失败降级 |
| [elevated-events.ts](D:/app_proxy/windows/src/elevated-events.ts) | 授权监听握手、管道重试和事件边界 |
| [msix.ts](D:/app_proxy/windows/src/msix.ts) / [msix-child.ts](D:/app_proxy/windows/src/msix-child.ts) | request/receipt/TTL、固定变量传递及包内 spawn |
| [msix-storage.ts](D:/app_proxy/windows/src/msix-storage.ts) / [store.ts](D:/app_proxy/windows/src/store.ts) | LocalState、scope、实际路径、归属与配置锁 |
| [bridge.ps1](D:/app_proxy/windows/native/bridge.ps1) | 包解析/激活、原生身份、快捷方式及进程停止 |
| [install-events.ps1](D:/app_proxy/windows/native/install-events.ps1) / [events-task.ps1](D:/app_proxy/windows/native/events-task.ps1) | 提权 helper 安装、任务 ACL 和所有权核验 |
| [core.ts](D:/app_proxy/windows/src/core.ts) / [builtin.ts](D:/app_proxy/windows/src/builtin.ts) | binary 选择、网卡探测、配置恢复和随包校验 |
| [subscription.ts](D:/app_proxy/windows/src/subscription.ts) / [proxy.ts](D:/app_proxy/windows/src/proxy.ts) | 订阅获取/对齐和实际代理证据 |

新实现以本设计为验收依据。需要引用现有平台经验时重新读取相关文件，尤其是仍在迭代的 Guard 和自动识别；它们不形成历史数据/命令兼容承诺。行号不是固定接口，文件内容和提交才是证据。

**2. 从开源 launcher 提炼的机制**

PortableApps Launcher 基线 `5470210a6841c38950c348e1190ad92787b8b727`：借鉴 [Environment segment](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/Segments/Environment.nsh) 的声明式环境构造及 [segment 生命周期](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/Segments.nsh)。不采用目录来回搬运和按 EXE 名识别实例。

Prism Launcher 基线 `d06c8a742894229d4967fb0bf16b9bf40cd377d3`：借鉴 [LaunchTask](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/launch/LaunchTask.cpp) 的步骤与清理分离，以及 [InstanceCopyTask](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/InstanceCopyTask.cpp) 的独立实例身份；本设计保留空白数据默认值。它的 Minecraft/Java/账号能力不迁入。

这里只采用设计思路，尚未复制上游源代码。若实现阶段直接引入上游代码，需按具体文件许可证处理，不因它是开源就默认能任意混入。

**3. 官方技术依据**

| 来源 | 支撑的事实 | 不推导的结论 |
|---|---|---|
| [windows-rs](https://github.com/microsoft/windows-rs) | Rust 可访问 Windows、COM/WinRT API | 不代表 API 无 unsafe 或自动满足本产品权限模型 |
| [Command](https://doc.rust-lang.org/std/process/struct.Command.html) / [Windows 扩展](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html) | 参数、cwd、环境、平台创建选项 | 不保证任意应用按预期支持分身或代理 |
| [Tokio shutdown](https://tokio.rs/tokio/topics/shutdown) | 协作取消与任务结束组织 | 取消 future 不自动撤销已发生系统副作用 |
| [MSIX 调试激活命令](https://learn.microsoft.com/en-us/powershell/module/appx/invoke-commandindesktoppackage?view=windowsserver2025-ps) | 包上下文和虚拟化资源访问及其限制 | 不宣称通用正式激活或 AppContainer 等价 |
| [Named pipe 权限](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights) | 显式 DACL、实例创建权和 session 限制 | 不把随机管道名当授权 |
| [进程权限](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights) | 查询和终止权限有别 | 不把 PID 当永久身份 |
| [StartTraceW](https://learn.microsoft.com/en-us/windows/win32/api/evntrace/nf-evntrace-starttracew) | ETW session 创建及限制 | 不承诺零延迟和零事件丢失 |
| [reqwest ClientBuilder](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html) | 显式 HTTP client 策略能力 | 不假设库默认代理行为与现有实现一致 |
| [IFEO Debugger 机制](https://devblogs.microsoft.com/oldnewthing/20070702-00/?p=26193) | debugger 前置到命令行、调用方获得 debugger 进程信息 | 不推导透明替代创建、MSIX 或同 EXE 子进程兼容 |
| [进程创建标志](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags) / [调试退出行为](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-debugsetprocesskillonexit) | 调试范围和退出生命周期需要显式处理 | 不把防递归及调试脱离候选当作已通过实测 |

**4. 已决定的取舍**

| ID | 决定 | 理由和代价 |
|---|---|---|
| D01 | Rust 作为独立新产品实现 | 用户明确不要求旧版兼容；从空配置开始，没有迁移器、旧版回退或双后端 |
| D02 | 一个 store 一个用户态 coordinator | 集中状态和所有权；多一个轻量进程，需 IPC 恢复 |
| D03 | 先三个 crate、两个入口 | 分开领域/平台/产品，不提前拆大量 crate |
| D04 | 首版保留有限 MSIX PowerShell | 保留实机已验证路径；不能宣称完全无脚本依赖 |
| D05 | 原版/分身和品牌分开建模 | 模板扩展不污染实例模式 |
| D06 | 默认 Guard stop_unproxied | 保持当前关闭误启动的保护意图，明确重启失败状态 |
| D07 | 事件监听高权限，决策/应用低权限 | 高权限接口小；安装/升级需前台授权 |
| D08 | 机密 ACL 存储，导出排除 | 与现有能力对齐；不宣称加密隔离 |
| D09 | 全新格式、UUID、数据根与资源命名空间 | 只分配自有独立数据，不读取旧 scope、目录和任务；数据模式只保留 original/isolated |
| D10 | sing-box 为外部程序依赖，发行包不携带，缺少时一键安装 | 只复用程序文件，全部代理进程用自有配置自行启动，不接入其他工具已运行的服务 |
| D11 | GUI 后置，CLI/menu 共用 IPC | 先验证系统兼容和流程；避免 GUI 反向绑架后端 |
| D12 | Unknown 不能自动重试副作用 | 降低重复实例风险；少数情况需要用户核对 |
| D13 | Codex/Claude 代理实例 Guard 默认开启，原版与分身都使用事件及实例检查 | 只处理已管理实例；共用 LaunchEngine，监听安装仍需前台授权 |
| D14 | IFEO 已取消（2026-09-20），不再是实现或发布门槛 | 用户明确取消；移除相关注册、启动入口和调试创建，仅保留系统已有 Debugger 的只读拒绝 |
| D15 | 运行中代理故障提示并保留应用 | 用户明确选择；不自动直连，新的代理绑定启动仍需验证通过 |
| D16 | 按 andrej-karpathy-skills 原则从简设计与实现 | 用户明确要求；仅为当前需求增加选项/接口，按实际流程逐步实现并验证，详细草案不等于必须提前实现的功能清单 |
| D17 | 同一 store 的多个代理共用一个自有 sing-box 进程 | 用户确认采用；多个 inbound/outbound 通过入口标签路由，内核故障或重启可能影响全部代理 |
| D18 | 不做跨目录升级及 Windows 登录任务被删除后的修复 | 2026-09-20 用户明确收敛范围；保留固定目录使用、正常登录自启和未完成操作恢复，不增加自愈机制 |

**5. 必须通过实验关闭的问题**

| 问题 | 验证方法 | 未通过时的处理 |
|---|---|---|
| Rust package-child 能否稳定在目标 MSIX 上下文启动 | 测试包 + 当前 Codex/Claude 原版/分身、包更新、LocalState | 修订包适配；未验收前不发布该能力，不以 Node 后端作为替代 |
| 原生身份读取与 WMI 命令行一致性 | 受控 child 系统信息、不同令牌/session、短命进程 | 不可读统一 unknown，不能扩大终止范围 |
| ETW session/pipe 的低权限客户端约束 | 假 server、重启、队列溢出、跨 session、权限拒绝 | 返回 degraded/needs_authorization，保持已授权扫描降级 |
| TLS 栈与实际 Windows 证书信任 | 本地 CA、企业根、错误证书、明确代理 | 固定通过测试的 TLS backend/feature，不能忽略验证 |
| YAML 库及协议映射是否满足格式契约 | 脱敏 fixtures、六协议 check、明确拒绝项 | 标记未支持，不能声称协议完整兼容 |
| 发行性能及最低 OS build | 固定机器/版本冷热基线与实机矩阵 | 发布说明明确支持范围，修正技术选择后再放行 |

上述是实施验证门槛，不要求用户现在提供额外信息，也不阻止完成本轮设计。涉及到的具体版本应在实际实施时重新验证。
