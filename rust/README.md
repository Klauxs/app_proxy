**App Proxy Rust 版设计**

状态：基础平台、配置/存储、认证管道、协调进程、实例配置、共享 sing-box 管理与启动 CLI 已实现。启动流程已接入缺失内核安装、共享代理扩容确认、查询与取消；MSIX 已接入包内 helper 和持久回执恢复。Codex/Claude 直连双分身已实测；账户与代理隔离、Guard/IFEO、中文菜单及完整验收仍待完成，不能作为正式启动器使用。详见 [验证记录](D:/app_proxy/rust/TEST-RESULTS.md) 和 [功能进度](D:/app_proxy/rust/IMPLEMENTATION.md)。

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

`status` 首次运行在 `%LOCALAPPDATA%\AppProxyRust` 创建独立 Rust 数据目录，之后连接或启动同一 store 的普通权限协调进程。可用 `--home <绝对路径>` 指定开发测试目录；已有非空未知目录或坏配置不会被重置。当前返回基础状态和实体数量，`phase` 为 `bootstrap`，不代表代理或 Guard 已运行。没有资源和已开启 Guard 的配置时，协调进程在最后一个请求结束后空闲 30 秒退出。配置编辑和应用启动均有持久请求记录，支持去重与查询。开发版 IPC 为 2.9，CLI 与 host 必须成套使用。

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

创建默认原版，必须选择 `--direct` 或 `--proxy <已登记代理ID>`；普通 EXE 使用 `--exe <绝对路径> --adapter codex|claude|chromium|environment`，只有已支持的 Codex/Claude 模板允许分身。应用位置和分身存储自动解析，不复制登录数据。移除只删除登记，保留数据；已有系统集成时先要求清理。每次写入前会输出请求编号，响应中断后查询原编号，不自动重新创建。首次登记应用和创建实例是两个请求，实例创建失败可能保留应用记录。列表为摘要，显示名最多 256 字符，不含参数、环境值和代理凭据；不是完整配置导出。Guard 授权仍待实现。

实例启动入口（普通 EXE / 已支持的 MSIX）：

```powershell
.\target\debug\app-proxy.exe launch <实例ID>
.\target\debug\app-proxy.exe launch <实例ID> --request-id <请求ID> --json
.\target\debug\app-proxy.exe launch inspect <请求ID> --json
.\target\debug\app-proxy.exe launch cancel <请求ID> --json
```

启动会先准备绑定代理，失败不改直连。缺少 sing-box 时在原流程提示安装；新增代理需要重启共享内核时先列出具体影响，确认后才执行。JSON/非交互模式通过 `requires_action` 和退出码 5 返回待处理事项，不自动安装或应用重启计划。只有本次前台请求已明确失败且未创建应用，依赖修复成功后才继续启动；期间配置改变会拒绝继续。

相同请求编号查询历史结果，不重复安装或创建应用；结果不明时保留编号，使用 `launch inspect` 查询。Ctrl+C 请求取消尚未创建的应用，并阻止当前流程的后续动作；已创建的应用会保留。已接受的内核切换仍需按其原请求编号查询结果。MSIX 通过一次性包内 helper 创建并保存回执；隔离存储包的请求位于自有 LocalState 命名空间。桥接退出不代表应用已创建，消费中或回执缺失保持未知。当前已验证普通 EXE 夹具、真实 Claude 过期 helper 回执，以及 Claude/Codex 各两个直连分身并存和独立目录写入；Codex 原版共存及重复启动复用通过。登录与代理隔离仍待验收；Guard/IFEO 尚未实现，启动成功不代表保护生效。

Guard 纠正执行服务已实现并通过测试：只针对已登记且归属明确的误启动主进程，先关闭再准备代理；失败保持关闭，已正确使用代理的应用不因网络故障被关闭。自动监控、Guard 授权和 IFEO 尚未接入，当前 CLI 启动不代表这些保护已生效。

内部只读扫描已能排除未管理原版、识别参数合规及误启动，并保留运行会话的历史绑定。扫描不创建分身目录，不自行启用保护或关闭进程；未决启动、多个主进程或身份不明时阻止纠正建议。

Guard 配置与诊断入口：

```powershell
.\target\debug\app-proxy.exe guard status <实例ID> --json
.\target\debug\app-proxy.exe guard enable <实例ID> --json
.\target\debug\app-proxy.exe guard disable <实例ID> --json
```

`status` 同时返回启用意图、实际状态、监听/IFEO 组件状态和只读进程观察。当前尚未接入提权组件部署，因此启用后显示 `needs_authorization`；已有集成登记仅显示 `unverified/blocked`，不会仅凭配置声明保护已生效。只管理分身时 IFEO 为 `not_applicable`。扫描繁忙时照常返回组件状态，并以 `GUARD_SCAN_BUSY` 表示本次没有新的进程观察。

`enable/disable` 使用持久配置请求，未改变目标状态时不重复提交。启用意图保存后若组件未完成，输出 `requires_action` 并返回退出码 5；已有 IFEO 登记时停用返回 `INTEGRATION_CLEANUP_REQUIRED` 并保留配置。Codex/Claude 代理实例创建、克隆及绑定也会在保存成功后明确报告这项待处理状态，不因退出码 5 重复创建实例。状态查询成功本身返回 0，不代表保护 active。若保存后的状态查询失败，会保留原请求回执并返回 6。组件安装、前台 UAC、ETW 和自动纠正触发仍待接入。

手动代理配置入口（HTTP/SOCKS5；本地入口自动分配）：

```powershell
.\target\debug\app-proxy.exe proxy create --name "工作代理" --protocol http --host 127.0.0.1 --port 8080
.\target\debug\app-proxy.exe proxy list --json
.\target\debug\app-proxy.exe proxy show <代理ID>
.\target\debug\app-proxy.exe proxy rename <代理ID> "新名称"
.\target\debug\app-proxy.exe proxy update <代理ID> --protocol socks5 --host 127.0.0.1 --port 1080 --no-auth
.\target\debug\app-proxy.exe proxy remove <代理ID>
.\target\debug\app-proxy.exe proxy request <请求ID> --json
```

认证使用 `--username <用户名> --password-stdin`，密码通过重定向标准输入传入，不接受密码命令参数；读取 UTF-8 并去除一个末尾换行。当前没有交互密码输入框。更新替换完整上游和认证，必须明确选择无认证或提供认证，避免遗漏参数时清空旧凭据。改名和更新保持 ID、本地入口与实例绑定；列表只显示认证是否已配置。密码保存在受保护的独立文件内，manifest 和持久化请求记录仅引用其 ID。移除要求实例及下载网络均不再引用此配置，保留秘密文件。

修改当前运行的共享内核所用代理时，`proxy update` 先生成并检查候选配置，列出受影响的代理及其绑定实例，再提示是否应用（默认返回）。明确指定 `--apply-to-running` 可同意这次变更；JSON/非交互默认只返回计划并以退出码 5 表示尚未应用。改名不触发重启。删除活动代理及已退出内核的恢复集合编辑仍待后续流程接入。启动前必须先恢复已接受的配置事务，再核对候选配置，避免中断写入在内核启动后悄悄生效。

候选准备不会修改当前代理；确认针对具体计划 ID，期间配置或原进程状态改变会拒绝陈旧计划。执行使用原内核程序，检查全部入口归属并验证被修改的出口；失败时恢复旧 generation，只要至少一个旧出口仍可用便保留恢复后的共享内核。原有故障出口不会拖停其余可用代理。所有旧出口都无法通过时保留旧配置并报告代理未恢复，不关闭应用。创建结果无法核对时保留待处理状态，不按进程名猜测或盲目再启动。

`discover sing-box` 自动探测 store 内完整版本目录、绝对 PATH、Scoop 和 WinGet Links 中的程序，执行 version 并输出位置/版本/来源，不创建 store，不接入外部服务。具体代理配置仍须执行 check；此只读命令不代表代理可用。探测子进程每次限时 3 秒、总异步等待预算 20 秒；同步文件系统访问（例如网络盘）仍受 Windows I/O 超时约束。只接受稳定版本，Scoop 的转发 shim 不作为内核执行。

共享内核的开发入口（需要已有代理配置）：

```powershell
.\target\debug\app-proxy.exe core start <代理ID> [其他代理ID] --required <本次检查的代理ID>
.\target\debug\app-proxy.exe core status --json
.\target\debug\app-proxy.exe core stop
.\target\debug\app-proxy.exe core request <请求ID> --json
.\target\debug\app-proxy.exe core install
.\target\debug\app-proxy.exe core cancel <安装请求ID>
.\target\debug\app-proxy.exe core apply-update <已检查的计划ID>
.\target\debug\app-proxy.exe core recover-update <中断的计划ID>
```

`start` 默认检查第一个代理，使用保存的 HTTPS 健康目标与允许状态码；只创建本工具拥有的进程，支持共享入口。请求只包含已有入口时直接复用；缺少新增代理时，先检查原集合与请求集合的并集配置，展示原入口中断影响和新增入口，再确认重启（默认返回）。可用 `--apply-to-running` 明确同意本次扩容，JSON/非交互默认返回计划及退出码 5，也可随后运行 `core apply-update <计划ID>`。扩容不删除原入口、不改保存的代理配置及 revision；检查失败恢复旧集合。已有代理的上游编辑使用上述更新流程。`stop` 停止自有共享内核，保留应用。写请求先持久化再执行，客户端断开仍继续；终态保留 7 天，未决记录保留到核对处理。同编号只返回历史结果，不能将历史 Ready 当成当前健康。`status` 核验进程和端口归属，并显示最近重配置计划与阶段，不执行网络请求。协调进程在有后台任务、活动内核或未决请求时不空闲退出；后台任务不占短连接槽位，查询与取消保持可用。未知启动结果不自动重放。应用启动许可已接入普通 EXE 启动；活动集合移除和持续故障通知仍待实现。

重配置期间普通启动、停止及配置写入会被拒绝，避免破坏恢复依据。`recover-update` 核对已有 journal：提交意图已保存时只完成配置提交；切换或回滚中断时按已记录的精确进程身份尝试恢复旧代理；没有进程身份的 Starting 仍需进一步核对，当前不会盲目重放。核对成功会更新原请求及相关恢复请求的回执。仅准备好的计划可以查询/恢复其准备回执，不因此开始切换。

交互终端中，`core start` 确认缺少程序后提供“安装并继续 / 返回”，失败提供“重试 / 返回”。选择安装才开始从固定[官方 1.14.1 发布](https://github.com/SagerNet/sing-box/releases/tag/v1.14.1)下载，校验 zip、EXE、DLL 和 LICENSE，version/check 通过后发布到 `<store>/bin/sing-box/1.14.1/`；无路径选择。下载明确直连，总时限 600 秒、连接 10 秒、读取停滞 20 秒，展示阶段与下载字节；并发请求共享一次尝试。JSON/重定向输入不会自动下载，可显式执行 `core install`。

安装等待时 Ctrl+C 会提交取消请求；也可用 `core cancel <安装请求ID>`，最终状态以原请求查询为准。取消当前请求不撤销其他已授权安装，发布已经完成时可能返回 Installed，但退出的原流程不会继续启动。单独安装成功不启动 core 或应用。当前固定版本已验证 HTTP/SOCKS5 及安装链路，其他订阅协议及完整发行验收仍待实现。

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
