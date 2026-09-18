**App Proxy Rust 版详细设计**

状态：设计草案，可进入实现评审。日期：2026-09-18。本目录目前只有设计和示例，没有 Rust 实现、可执行程序或编译结果。

目标是全新设计一套 Rust 实现的实例启动器：普通用户选择应用、原版或空白实例以及代理即可启动；平台层处理进程身份、MSIX、快捷方式和 Guard。代理协议仍由 sing-box 实现。按用户要求，不承担旧版配置、命令、数据目录、任务或快捷方式兼容，不提供旧版迁移和回退。现有实现只作为功能经验与平台行为的参考。

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
| [manifest.json](D:/app_proxy/rust/examples/manifest.json) | 无凭据的配置示例，含原版和独立实例 |
| [launch-events.ndjson](D:/app_proxy/rust/examples/launch-events.ndjson) | 启动事件协议示例，不是实测日志 |

**关键决定**

- 首发目标 Windows x64，先保持中文菜单与 CLI；GUI 和 macOS 实现后置，平台边界从第一版建立。
- 三个 crate、两个普通发行入口，共用一个启动核心；用户态 coordinator 统一管理配置写入、启动任务和 Guard。
- Rust 直接实现常规 Windows 集成。MSIX 首版保留受限 PowerShell 桥接，包内执行和回执改用 Rust helper。
- 应用安装信息、适配模板、运行实例分别建模；改名不改 ID 或数据目录；复制配置默认创建空白数据。
- Guard 默认沿用“检测到未按要求代理的进程就关闭”的保护意图；代理不可用时明确报告停止且阻止重启。首版不提供静默保留直连的替代策略。
- 只有完整身份确认后才能停止进程；未知结果不重复启动；外部 sing-box 永远不由本工具停止。
- Rust 新格式从 schema_version=1 开始，使用独立格式标识、数据根和系统资源命名空间，首次启动从空配置开始。

**基线变化**

本次以 `D:\app_proxy` 的 `main`、提交 `a38a29865578035ba8ad0d85d0e319bbb63880e2` 及读取时工作区为基线。当前已经有 Codex/Claude 自动识别、串联代理创建和桌面预设默认 Guard；这些属于保留能力。更早的 [开源调研](D:/app_proxy/Launcher-开源调研与流程建议.md) 对创建菜单的描述早于该提交，以本文档为准。

本文固定产品与接口边界，明确需要验证的平台风险。crate 版本、MSRV、Windows 最低 build、性能阈值在首轮技术验证后锁定；此处没有未经测试的兼容性和性能保证。
