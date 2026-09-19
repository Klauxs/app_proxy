**App Proxy Rust 版设计**

状态：基础平台、配置/存储、认证管道、协调进程、启动模板、实例数据/配置编辑、安装解析及共享 sing-box 初始生命周期已实现。2026-09-20 已接入 core 启动/停止/状态/请求查询 CLI。仍缺少一键安装、代理编辑菜单、应用实例启动与 Guard，不能作为正式启动器使用。详见 [验证记录](D:/app_proxy/rust/TEST-RESULTS.md) 和 [功能进度](D:/app_proxy/rust/IMPLEMENTATION.md)。

**构建与验证**

需要 Windows x64、Rust 1.98.1 和 Visual Studio C++ Build Tools。`Cargo.lock` 已固定依赖。常规环境在本目录执行：

```powershell
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
.\target\debug\app-proxy.exe discover claude
.\target\debug\app-proxy.exe discover sing-box
.\target\debug\app-proxy.exe probe process --debug-detach
.\target\debug\app-proxy.exe probe package claude
.\target\debug\app-proxy.exe status --json
```

本机 Rust 安装在项目 `.tools` 内，未修改系统 PATH；可使用 `./scripts/cargo.ps1 build --workspace --locked`。传递 Cargo 的 `-p` 或 `--` 等参数时用数组，避免 PowerShell 参数绑定冲突，例如 `./scripts/cargo.ps1 -CargoArgs @('clippy','--workspace','--all-targets','--locked','--','-D','warnings')`。

`status` 首次运行在 `%LOCALAPPDATA%\AppProxyRust` 创建独立 Rust 数据目录，之后连接或启动同一 store 的普通权限协调进程。可用 `--home <绝对路径>` 指定开发测试目录；已有非空未知目录或坏配置不会被重置。当前只返回基础状态和实体数量，`phase` 为 `bootstrap`，不代表代理或 Guard 已运行。没有资源和已开启 Guard 的配置时，协调进程在最后一个请求结束后空闲 30 秒退出。配置编辑、持久化请求去重和结果查询已接入 coordinator RPC 与实例 CLI；菜单和应用运行 journal 仍待实现。开发版 IPC 为 major 2，CLI 与 host 必须成套使用。

实例配置命令已可使用（仅保存配置，不启动应用或启用保护）：

```powershell
.\target\debug\app-proxy.exe instance create --preset claude --data isolated --direct --name "Claude 分身"
.\target\debug\app-proxy.exe instance list --json
.\target\debug\app-proxy.exe instance clone <实例ID> --name "新分身"
.\target\debug\app-proxy.exe instance rename <实例ID> "新名称"
.\target\debug\app-proxy.exe instance bind <实例ID> --direct
.\target\debug\app-proxy.exe instance remove <实例ID>
.\target\debug\app-proxy.exe instance request <请求ID> --json
```

创建默认原版，必须选择 `--direct` 或 `--proxy <已登记代理ID>`；普通 EXE 使用 `--exe <绝对路径> --adapter codex|claude|chromium|environment`，只有已支持的 Codex/Claude 模板允许分身。应用位置和分身存储自动解析，不复制登录数据。移除只删除登记，保留数据；已有系统集成时先要求清理。每次写入前会输出请求编号，响应中断后查询原编号，不自动重新创建。首次登记应用和创建实例是两个请求，实例创建失败可能保留应用记录。列表为摘要，显示名最多 256 字符，不含参数、环境值和代理凭据；不是完整配置导出。代理创建、启动及 Guard 授权仍待后续实现。

`discover sing-box` 自动探测 store 内完整版本目录、绝对 PATH、Scoop 和 WinGet Links 中的程序，执行 version 并输出位置/版本/来源，不创建 store，不接入外部服务。具体代理配置仍须执行 check；此命令不代表代理可用，也还没有接入安装提示。探测子进程每次限时 3 秒、总异步等待预算 20 秒；同步文件系统访问（例如网络盘）仍受 Windows I/O 超时约束。只接受稳定版本，Scoop 的转发 shim 不作为内核执行。

共享内核的开发入口（需要已有代理配置和可用 sing-box 程序）：

```powershell
.\target\debug\app-proxy.exe core start <代理ID> [其他代理ID] --required <本次检查的代理ID>
.\target\debug\app-proxy.exe core status --json
.\target\debug\app-proxy.exe core stop
.\target\debug\app-proxy.exe core request <请求ID> --json
```

`start` 默认检查第一个代理，使用保存的 HTTPS 健康目标与允许状态码；只创建本工具拥有的进程，支持共享入口。运行配置需要改变时返回需确认，尚未实现切换。`stop` 停止自有共享内核，保留应用。启动/停止先持久化请求再执行，客户端断开仍继续；终态保留 7 天，未决记录保留到后续维修处理。同编号只返回历史结果，不能将历史 Ready 当成当前健康。`status` 核验进程和端口归属，不执行网络请求。协调进程在保存有活动内核或未决请求时不空闲退出；未知启动结果不自动重放。该入口尚未包含应用启动许可、持续故障通知及安装提示。

`probe package claude` 仅在已安装 Claude 的包身份下启动本产品测试 helper，验证回执和独立临时目录读写，不启动 Claude 界面或修改其登录数据。`probe process --debug-detach` 验证调试创建和脱离，**不代表真实 IFEO 注册或 Electron 子进程兼容性已经通过**。

目标是全新设计一套 Rust 实现的实例启动器：普通用户选择应用、原版或空白实例以及代理即可启动；平台层处理进程身份、MSIX、快捷方式和 Guard。代理协议仍由 sing-box 实现。按用户要求，不承担旧版配置、命令、数据目录、任务或快捷方式兼容，不提供旧版迁移和回退。现有实现只作为功能经验与平台行为的参考。

**从简原则**

按用户指定的 [andrej-karpathy-skills](https://github.com/multica-ai/andrej-karpathy-skills) 原则推进：先明确问题，采用满足需求的最小实现，只修改当前任务需要的内容，并用可验证结果判断完成。每个新增选项、模块或抽象都必须能说明它解决的当前需求；没有实际需求就不加。

- 普通流程只让用户选择应用、原版/空白实例和代理。能自动确定的程序位置、目录、端口等不增加询问；实际歧义再提示。
- 缺少 sing-box 时就在当前流程提供“安装并继续 / 返回”，程序管理安装位置；失败提供“重试 / 返回”。
- Guard 是一项保护功能，Codex/Claude 受管原版默认包含 IFEO；只创建分身时不注册 IFEO、不处理未管理原版。组件状态供诊断，不扩展成一组日常开关，不增加默认路由编辑器。
- 共用一套启动流程和一个状态所有者；内部业务优先使用普通函数与结构体，仅为真实的系统边界或故障测试需要引入接口。不建立通用插件、工作流或平台框架。
- 按当前功能需要实现命令、字段和恢复步骤。详细章节中的接口、CLI 清单及目录划分是设计参考，不要求提前逐项搭空壳；新增范围需另行讨论。
- 保留与真实副作用有关的检查：不重复启动、不误杀进程、不覆盖他人配置、IFEO 不递归、配置失败可恢复。测试围绕这些行为与实际启动闭环，不为简单实现机械配套测试。

目标日常链路：选择应用与实例 → 选择/配置代理 → 自动查找 sing-box，缺少则询问安装 → 准备代理 → 启动应用。Guard 在创建流程中完成必要授权；运行中代理故障只提示、保留应用。用户已确认开始实现，目前交付上述基础功能和验证入口；后续按实际验收结果逐步补齐。

**阅读入口**

| 文档 | 内容 |
|---|---|
| [01-architecture.md](D:/app_proxy/rust/docs/01-architecture.md) | 范围、进程与 crate 划分、依赖、接口、关键决策 |
| [02-model-and-storage.md](D:/app_proxy/rust/docs/02-model-and-storage.md) | 应用/模板/实例模型、新配置格式、存储、环境变量、事务 |
| [03-launch-and-guard.md](D:/app_proxy/rust/docs/03-launch-and-guard.md) | 启动状态机、并发、取消与恢复、Guard 策略和资源所有权 |
| [04-windows-platform.md](D:/app_proxy/rust/docs/04-windows-platform.md) | 原生进程、MSIX、ETW、权限、管道、快捷方式、升级 |
| [05-proxy-and-subscriptions.md](D:/app_proxy/rust/docs/05-proxy-and-subscriptions.md) | sing-box 发现/复用、托管内核、订阅、联网探测与回滚 |
| [06-product-and-protocol.md](D:/app_proxy/rust/docs/06-product-and-protocol.md) | 用户流程、CLI、IPC、事件、错误及诊断 |
| [07-implementation-and-validation.md](D:/app_proxy/rust/docs/07-implementation-and-validation.md) | 从零实现的批次、接口验收、测试和发布门槛 |
| [08-evidence-and-decisions.md](D:/app_proxy/rust/docs/08-evidence-and-decisions.md) | 当前源码基线、上游来源、决定及待验证事项 |
| [09-ifeo-launch-interception.md](D:/app_proxy/rust/docs/09-ifeo-launch-interception.md) | IFEO 启动前接管、默认实例、防递归、子进程、系统注册及验收 |
| [manifest.json](D:/app_proxy/rust/examples/manifest.json) | 无凭据的配置示例，含原版和独立实例 |
| [launch-events.ndjson](D:/app_proxy/rust/examples/launch-events.ndjson) | 启动事件协议示例，不是实测日志 |

**关键决定**

- 首发目标 Windows x64，先保持中文菜单与 CLI；GUI 和 macOS 实现后置，平台边界从第一版建立。
- 三个 crate、两个普通发行入口，共用一个启动核心；用户态 coordinator 统一管理配置写入、启动任务和 Guard。
- Guard 包含 IFEO 启动前接管和 ETW 启动后检查，Codex/Claude 代理预设默认开启。IFEO 仅在原版已登记且开启 Guard 时注册并路由原版；分身通过专用入口及实例检查保护。实际所需组件启用仍需授权和兼容验收，部分失败明确显示保护不完整。
- 发行包不携带 sing-box；优先复用本机程序文件，缺少时一键安装。所有代理进程均由本工具用独立配置启动，不接入其他工具已运行的服务。
- 同一 store 的多个代理共用一个自有 sing-box 进程，以不同本地入口路由到对应出口；多个应用实例可以绑定同一代理。
- 运行期间代理故障只提示并保留应用，不自动改直连；新的代理绑定启动仍要求验证通过。
- Rust 直接实现常规 Windows 集成。MSIX 首版保留受限 PowerShell 桥接，包内执行和回执改用 Rust helper。
- 应用安装信息、适配模板、运行实例分别建模；改名不改 ID 或数据目录；复制配置默认创建空白数据。
- Guard 默认沿用“检测到未按要求代理的进程就关闭”的保护意图；代理不可用时明确报告停止且阻止重启。首版不提供静默保留直连的替代策略。
- 只有完整身份确认后才能停止进程；未知结果不重复启动；外部 sing-box 永远不由本工具停止。
- Rust 新格式从 schema_version=1 开始，使用独立格式标识、数据根和系统资源命名空间，首次启动从空配置开始。

**基线变化**

本次以 `D:\app_proxy` 的 `main`、提交 `a38a29865578035ba8ad0d85d0e319bbb63880e2` 及读取时工作区为基线。当前已经有 Codex/Claude 自动识别、串联代理创建和桌面预设默认 Guard；这些属于保留能力。更早的 [开源调研](D:/app_proxy/Launcher-开源调研与流程建议.md) 对创建菜单的描述早于该提交，以本文档为准。

本文记录已确定的产品规则；详细接口按实际实现需要收敛，不为未来假设预留功能。crate 版本、MSRV、Windows 最低 build、性能阈值在首轮技术验证后锁定；此处没有未经测试的兼容性和性能保证。
