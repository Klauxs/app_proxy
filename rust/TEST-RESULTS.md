**Rust M0 首批实现验证 — 2026-09-19**

本记录只描述已运行的代码，设计文档不等同于已实现功能。当前完成 M0 的进程创建/身份及包内 helper 验证部分，M0 尚未全部通过。

**环境**

- Windows 11 专业工作站版，10.0.26200，x64，普通用户令牌。
- Rust 1.98.1（48a229cea，2026-09-01），MSVC 工具 14.36.32532。
- Rust/rustup 位于项目 `.tools`，未修改系统 PATH；发行目标仍不包含 Rust 工具链。
- Claude MSIX：`Claude_2.2553.1.0_x64__pzs8sxrjxfjjc`，主 AppId `Claude`。
- 依赖由 `Cargo.lock` 固定；没有安装、启动或复用 sing-box。

**已通过**

| 验证 | 实际结果 |
|---|---|
| Workspace 构建 | 三个 crate，CLI 与 GUI-subsystem host 均构建成功 |
| 自动测试 | 7 项通过；另 1 项需要 Claude 安装的实机测试默认忽略，本机单独执行通过 |
| 普通进程创建 | 空参数、中文、空格、引号、尾反斜杠及 Unicode 参数从真实 child 回执逐项核对一致；cwd 一致 |
| 环境 | set 大小写匹配、空字符串、unset 经真实子进程验证，父进程环境未改变；含混补丁/NUL 被拒绝 |
| 进程身份 | PID、创建时间、SID、session、映像路径及文件身份一致；伪造创建时间不能停止进程，原进程保持存活 |
| 精确清理 | 使用确认身份和持有句柄停止本轮受控 helper，确认退出；没有按名称或进程树清理 |
| 调试创建候选 | DEBUG_ONLY_THIS_PROCESS → 初始调试事件 → 脱离；创建线程退出后 child 仍存活并写出回执 |
| 请求约束 | 过期请求、错误包身份不写回执；同一请求重复执行拒绝且不覆盖第一次回执 |
| Claude 包识别 | 从当前用户包登记解析 full-trust 主程序与文件系统虚拟化属性 |
| Claude 包内 helper | Rust host 在真实 Claude 包身份下运行；包内写入 LocalState 的独立测试目录，包外成功读回匹配 nonce 的数据 |
| 临时资源 | 最后回读未发现遗留的本轮 host 进程或 `AppProxyRust-M0-*` 临时目录 |

验证期间发现 PowerShell 无法读取仍由 Rust 以写模式持有的临时脚本，已改为关闭写句柄后调用，并由 Claude 实机测试覆盖该链路。正常错误输出不包含原始 PowerShell stderr 或环境值。

**重跑命令**

在 `rust` 目录执行（有常规 Rust 环境时可直接使用 cargo）：

```powershell
.\scripts\cargo.ps1 build --workspace --locked
.\scripts\cargo.ps1 test --workspace --locked
.\scripts\cargo.ps1 -CargoArgs @('clippy','--workspace','--all-targets','--locked','--','-D','warnings')
.\scripts\cargo.ps1 -CargoArgs @('fmt','--all','--','--check')
.\scripts\cargo.ps1 -CargoArgs @('test','-p','app-proxy-app','--test','process_contract','--locked','claude_package_helper','--','--ignored')
```

**尚未验证 / 尚未实现**

- 未写 IFEO 注册表；调试创建实验不能证明真实注册匹配、防递归、Electron 同 EXE 辅助进程或父进程句柄语义兼容。
- 未启动真实 Claude 主程序，未验证原版/分身 UI、登录、实际网络请求或模型对话。包内 helper 成功只是原生包上下文能力的证据。
- 未实现 ETW listener、持久化启动状态机、代理/订阅/一键安装或用户菜单。store、命名管道与 bootstrap coordinator 的后续验证见下方追加记录。
- `SpawnSpec`/调试后端为 M0 代码；当前普通身份核验不替代 M1 的实例归属、未决结果 journal 和生产停止策略。
- 当前终止接口仅用于受控测试或失败启动的清理，尚无面向用户的“先正常关闭窗口”操作。
- M0 EnvPatch 的显式变量名只支持 ASCII，变量值和继承环境支持 Unicode；这不等于完整模板/代理环境合成已实现。

真实 IFEO 受控夹具和 ETW 验证仍待完成；应用级接管必须在相关平台实验通过后接入。实现过程中若需改变已确认的产品行为，再回到设计讨论。

**配置模型与存储追加验证**

基础模型 13 项测试通过；单所有者存储 10 项 Windows 测试通过。模型拒绝未知字段、无效引用、受管参数/环境冲突与未管理原版的 IFEO 登记。存储测试覆盖版本冲突、备份、损坏/超大文件保留、机密引用、文件占用导致的替换失败、目录及文件 ACL、继承标志、junction 拒绝，以及权限变化时写秘密前拒绝。两个功能均独立审查、修复后复审通过。

当前节点类型仅手动 HTTP/SOCKS5。存储尚未接入日常菜单；配置恢复界面、运行 journal、实例启动和代理管理仍未完成。前述 M0 记录仍只代表实验范围。

**IPC 与协调进程追加验证**

已通过 4 项真实命名管道测试、2 项帧测试，以及协调进程的 2 项协议测试和 4 项跨进程测试。跨进程场景包括：六个 CLI 同时创建全新 store 并取得同一 PID/epoch；退出后重新启动仍使用同一 store 并读取新 revision；Unicode/空格/尾部分隔符路径；坏配置原样保留；最后一个请求结束后空闲 30 秒退出；独立 PowerShell 进程连续快速建立并关闭 20 次连接后仍由原 owner 提供服务。协议测试覆盖版本/store/session/epoch 拒绝和慢客户端隔离。

后台 host 使用固定 `serve --home` 入口，通过原生 `CreateProcessW` 禁止句柄继承。原先 std::Command 默认继承捕获输出的句柄，导致 CLI 调用方等待 host 退出才收到 EOF；竞启测试捕获了该问题，修复后 CLI 在 host 存活时即可返回。Rust stable 的相关限制可查 [CommandExt::inherit_handles](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html#tymethod.inherit_handles)。

本批只实现只读 status 和生命周期。写请求去重、运行 journal、应用启动恢复、事件缓冲和实际 Guard/代理任务仍待实现；不以 bootstrap 状态表示保护生效。

**模板与实例目录追加验证**

7 项模板测试覆盖原版清除继承目录变量、Codex/Claude 空白实例参数、环境变量模板、直连/代理、IPv6、单次路径变量展开、秘密引用与环境补丁限额。6 项目录测试覆盖原版不创建目录、空白创建、改名复用、移除登记保留数据、陌生目录/错误归属拒绝、根目录句柄保留、junction 拒绝，以及使用隔离 LocalState 夹具时不同 store 的命名空间分离。两项功能均独立审查通过。

2 项跨进程测试把污染的继承代理/实例目录变量送入独立 worker，经过真实模板合成与目录准备，再由 PowerShell 回执核验 argv，原生 Rust 子进程核验环境值。PowerShell/.NET 对空变量的观察可能与原生 API 不同，empty/unset 契约以原生回执判断。两个 helper 默认列为 ignored，由父测试显式选择并执行；不添加新的发行程序。此测试没有启动真实 Codex/Claude，不证明 Electron 全组件隔离、MSIX 包内目录可访问或实际网络经过代理。

**共享 sing-box 配置追加验证**

2026-09-19：纯配置生成器通过 4 项契约测试及独立审查。每个活动 profile 生成一个回环 HTTP 入口和指定上游出口；按稳定 UUID 排序、重复引用合并，改显示名/配置 revision 不改变内核配置。选中节点的密码只在生成时解析，不提供配置对象的 Debug/Display；无 direct/selector 出口，末尾规则拒绝未匹配流量，不设置系统代理。

显式运行 `singbox_process_contract` 通过，独立 reviewer 再次运行通过。实际 sing-box 1.14.1 完成生成配置的 `check`；一个新启动进程同时向本地 HTTP Basic 和 SOCKS5 认证夹具转发 CONNECT，两端均验证收到原始目标域名。测试增加的未匹配入口被拒绝；关闭 HTTP 上游后对应入口失败，另一个 SOCKS5 上游仍成功，未发生串线。仅证明配置有效及本地 TCP 转发，不证明外网 TLS 健康、真实应用流量、六种订阅协议或生产内核生命周期。

验证程序来自[官方 v1.14.1 发布](https://github.com/SagerNet/sing-box/releases/tag/v1.14.1)，存放于忽略的 `.tools/sing-box-1.14.1-validation/`，不是产品安装或发行内容。下载 zip 的 SHA256 与发布 API 的 digest 一致：`5197f16d492d93202dc623622149a6ed040f8eca263128f91d603f2b901baa89`。解压的 EXE SHA256 为 `b838de45bd0b2e6ddbed1977e4745622f7dffab3b293807ff4c6b1b640fed909`，libcronet.dll 为 `3217c6260fbca5f16072e0b79735742f40109a63bb0ff88fd6b96dd6b54a2928`；包内 LICENSE 保留。此版本尚未通过完整协议/网络验收，不能据此声称一键安装版本已经完成验收。

开发者在仓库根目录复跑：

```powershell
$env:APP_PROXY_TEST_SING_BOX = Join-Path (Get-Location) '.tools/sing-box-1.14.1-validation/extracted/sing-box-1.14.1-windows-amd64/sing-box.exe'
& ./rust/scripts/cargo.ps1 -CargoArgs @('test','-p','app-proxy-app','--test','singbox_process_contract','--','--ignored')
```

此环境变量只用于显式集成测试，不是用户选择内核路径的产品选项。常规 workspace 测试默认忽略该外部依赖测试；发行验收须显式运行。字段依据：[HTTP 入口](https://sing-box.sagernet.org/configuration/inbound/http/)、[HTTP 出口](https://sing-box.sagernet.org/configuration/outbound/http/)、[SOCKS 出口](https://sing-box.sagernet.org/configuration/outbound/socks/)、[路由动作](https://sing-box.sagernet.org/configuration/route/rule_action/)、[上游域名解析](https://sing-box.sagernet.org/configuration/shared/dial/#domain_resolver)。
