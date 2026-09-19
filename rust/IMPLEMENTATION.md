**实现与验收跟踪**

目标：按已确认设计完整实现 Rust 版，每项功能经独立 review、修复与验证后单独 commit。此文件记录进度，不缩小设计范围；不把实验、编译或局部测试作为整个产品完成的证据。

最近完成：订阅预览协调服务及 IPC 2.13 通过独立 review；8 项新增默认回归和真实 sing-box 自有路由下载经独立复跑通过。预览有界、可取消、到期失效；stage 保留精确请求及失败原因，提交复用既有事务。全量 357 项通过（33 项顶层 ignored），clippy/fmt/diff 通过。提交主题 `feat(rust): coordinate bounded subscription previews and staging`，此前刷新/选择事务已提交 `46ee54d`。实现与验收范围见 `TEST-RESULTS.md`；CLI 导入/刷新/节点选择及完整菜单待续。

| 功能 | 当前状态 | 完成证据 / 下一项验证 |
|---|---|---|
| M0 普通进程/身份/调试创建候选 | 已实现，独立 review 通过 | `TEST-RESULTS.md`，普通/调试 child 契约测试；不等于完整 M0 通过 |
| MSIX 包内 helper | 生产执行链及恢复已接入，独立 review 通过；完整验收待续 | Claude/Codex 各两个直连分身同时运行、目录写入隔离及重复请求复用通过；Codex 原版共存通过；登录、实际代理及更新验收待续 |
| 配置模型 | 手动和订阅来源模型已实现，独立 review 通过 | 严格 JSON、引用/路径/Guard/IFEO 校验；HTTP/SOCKS5 与六协议订阅元数据/秘密引用，读取和提交校验完整 typed 秘密；刷新编辑入口待续 |
| 配置存储 | 基础存储已实现，独立 review 通过 | 归属/ACL、revision、锁、原子替换/上一份备份、不可变秘密与坏文件拒绝；10 项 store 测试；恢复界面与运行 journal 待实现 |
| IPC 传输 | 已实现，独立 review 通过 | 双向身份/普通权限/会话/映像验证、管道 ACL、1 MiB 帧与超时/取消；4 项真实管道 + 2 项帧测试 |
| coordinator | 引导、配置写、core 控制及普通 EXE 启动 RPC 已实现，独立 review 通过 | 单所有者、握手、去重/查询/取消；丢应答继续执行，任务/内核/未决请求保活；共享 core gate、应用退出只读维护已验证，完整未知副作用核对待续 |
| 启动模板 | 纯合成已实现，独立 review 通过 | 原版/分身参数环境、直连/代理、秘密引用、路径变量；7 项模板测试；不表示代理就绪或可以启动 |
| 实例数据目录 | 已实现，独立 review 通过 | store/LocalState 命名空间、归属/ACL/目录句柄、空白创建、保留数据；6 项目录测试 + 2 项模板子进程测试；真实包内访问待验证 |
| 实例配置编辑 | 已实现，独立 review 通过 | 已接入 coordinator 与 CLI；7 项规则 + 4 项真实 CLI 写测试，物理去重、保留数据及列表脱敏；菜单及外部集成清理待实现 |
| 实例 CLI / 列表摘要 | 已实现，独立 review 通过 | create/list/clone/rename/bind/remove/request，按 revision 和字节预算分页；普通 EXE 启动已有独立 launch 命令，保护授权待实现 |
| 手动代理配置 / CLI | 已实现，独立 review 通过 | HTTP/SOCKS5 create/list/show/update/rename/remove/request；自动入口、认证分存与脱敏、引用及活动 generation 保护；运行中更新已接确认/回滚，交互菜单待实现 |
| 共享 core 手动上游重配置 | 已实现，独立 review 通过 | 检查候选、影响预览/具体计划确认、原程序切换、失败回滚、持久化阶段/回执恢复；活动集合移除、未知 Starting 的完整核对及未来应用启动许可仍待续 |
| 共享 core 代理集合扩容 | 已实现，独立 review 通过 | 原集合并入新增入口、影响预览与明确确认、不改 manifest、失败恢复旧集合、已有子集复用同进程；真实 core/CLI 测试通过 |
| 配置请求恢复/去重 | 已实现，独立 review 通过 | 8 项事务测试覆盖 pending 两侧恢复、结果重放、文件占用、损坏及配置回退；已接入配置 IPC，查询可恢复纯配置 pending |
| 实例启动 | 普通 EXE 服务、RPC/CLI、持久状态、core 许可和跨 store 预留已实现；菜单待接入 | 去重/取消、实际夹具创建、外部占用拒绝、配置复核、双 journal 恢复及精确退出通过；真实 Codex/Claude、多实例完整验收、MSIX 与无可信回执的未知结果核对待实现 |
| 安装解析 | 已实现，独立 review 通过 | EXE 文件身份/别名、短期句柄、MSIX 稳定定位/旧计划拒绝；3 项安装 + 2 项桥接夹具测试，真实 Codex/Claude 只读发现通过；已接入登记，启动待实现 |
| 进程只读查询 | 已实现，独立 review 通过 | 原生快照提示、完整身份句柄/WMI 创建时间复核、有界查询/取消及 Windows argv；已接普通 EXE 启动前占用检查，Guard、旧包及祖先链识别待续 |
| 单进程实例归属 | 已实现，独立 review 通过 | 精确进程/EXE 硬链接、原版与分身目录物理身份、主/辅助/未知分类；4 项测试通过，全局占用扫描、旧 MSIX 身份核对、代理参数证据和 LaunchEngine/Guard 集成待续 |
| sing-box 管理/一键安装 | 初始生命周期、coordinator/CLI、一键安装/取消已实现，独立 review 通过；产品集成待续 | 自有共享进程；固定官方包校验、自动目录、交互确认/重试、并发安装、进度与取消；启动许可、重配置/回滚、未知副作用核对及持续故障通知待实现 |
| sing-box 配置生成 | 已实现，独立 review 通过 | 活动 profile 合并、固定入口→出口、末尾拒绝、HTTP/SOCKS5 认证及稳定配置；4 项规则测试和 1 项真实内核转发测试，已接入初始内核管理组件 |
| sing-box 程序发现/检查 | 已实现，独立 review 通过 | 自动发现/真实 version、有界子进程输出及 check；CLI discover sing-box 已接入，安装及生产运行待实现 |
| HTTPS 代理健康检查 | 已实现，独立 review 通过 | 显式指定入口、CONNECT/TLS/主机名验证、无重定向、10 秒/1 MiB、错误脱敏；6 项本地行为测试和真实 sing-box TLS/管理集成，已接初始内核管理，应用启动/运行监控待实现 |
| 订阅下载 | 传输层及预览协调服务已实现，独立 review 通过 | 明确路由、UA 回退、TLS/重定向限制、双 8 MiB 上限；自有已运行 core 归属核验、内存预览/分页/取消/TTL 与 stage；8 项新默认测试及真实路由下载通过；CLI 与就绪/安装流程待接入 |
| 订阅解析 | 六协议 typed Node、URI/Base64、Clash YAML、客户端文本与单 outbound 编译已实现，独立 review 通过 | 22 项专项、28 个真实 1.14.1 check 样例及 GET/POST 本地收包通过；有界 YAML 别名与层级、凭据引号/单次解码、严格字段/语义组合 |
| 订阅持久化与共享编译 | 已实现，独立 review 通过 | 10 项默认回归及真实 7 profile check，URL/节点秘密分存、严格绑定与损坏保留、目录脱敏、选中节点编译、旧启动摘要兼容 |
| 订阅 staging/刷新/选择事务 | 已实现，下载协调已接入，独立 review 通过；CLI 待续 | 来源 CAS、按名字保持身份/选择、秘密复用与严格重放、完整配置字节判断重启；共享 core 预览/确认/恢复和持久回执，6 项默认及真实 Shadowsocks 切换/回滚通过；IPC 2.12，预览入口 2.13 |
| IFEO | 注册/恢复平台层已实现并通过独立 review，入口待接入 | 受保护归属及父值备份、精确路径过滤、持久断点恢复、冲突保留；15 项专项测试通过。尚无生产调用方写入规则，真实匹配/防递归/子进程/完整接管待验收 |
| Guard/ETW | 监听 host、授权安装、普通侧监听监督、自动扫描与纠正触发已接入 | 调度/受控进程/状态/协议测试及独立 review 通过；真实提权事件链路、登录任务与 IFEO 仍待验收或实现 |
| 原生 ETW 进程事件平台层 | 已实现，独立 review 通过；实际 kernel provider 采集待授权验证 | 固定 provider/事件、TDH 命名属性、1024 项有界提示队列、丢失统计、会话归属及退出；默认 4 项及真实空会话控制测试通过，当前权限启用 provider 返回 Win32 5；未接入提权部署或事件管道 |
| 单向事件管道平台层 | 已实现，独立 review 通过；真实提权两端集成待续 | 独立名称及只读 logon ACL、双向用户/session/logon/映像/权限核对、严格消息和断线补扫；每批 128 条，队列送完才报告结束；5 项管道及 5 项 ETW 默认测试通过，部署与自动 Guard 尚未连接 |
| Guard 受保护 helper generation | 平台代码与独立 review 完成；真实提权部署待验收 | 自动 Program Files 目录、UAC 前源文件/父目录 pin 与独立身份/hash、同用户普通 issuer、管理员 owner/只读用户 ACL、不可变复制/回读；已接前台 UAC 固定安装入口，不表示实机已激活 |
| Guard 按需监听任务平台层 | 已实现，独立 review 通过；真实注册运行待验收 | 固定任务/保护目录 host/UUID action，管理员 owner、用户只读运行；完整定义/ACL、只创建不覆盖、当前 session RunEx、空闲删除；前台安装与 event-listen 已接，真实 UAC 和普通侧启动监督待验收 |
| Guard 生产监听与崩溃恢复 | 已实现，独立 review 通过；真实提权事件采集待验收 | 认证截止/心跳/断开退出，跨 generation 受保护 epoch journal 及原生空 trace 恢复；260 项全量与实机空 trace 检查通过 |
| Guard 前台监听组件授权 | 已实现，独立 review 通过；真实 UAC 待验收 | 交互 guard enable 内询问安装，固定来源/hex ticket/issuer、protected 意图先于任务注册、相同部署复用、取消等待与未知保留；只读状态共享 3 秒预算；JSON/后台不提权 |
| Guard 纠正执行服务 | 已实现，独立 review 通过；自动触发待接入 | 精确误启动主进程、持久一次性停止许可、全局限流、辅助残留等待及停止后代理失败禁止直连；9 项新增回归通过，不自动监听或注册 IFEO |
| Guard 只读扫描 | 已实现，独立 review 通过；监听接入待续 | 登记实例归属、历史会话绑定保留、未决/歧义保守处理，目录不创建；扫描结束复核 revision/未决请求，超时解析线程不累积 |
| Guard CLI / 授权状态边界 | 已实现，独立 review 通过；完整激活待续 | status/enable/disable、desired/实际/组件分离；只登记分身 IFEO 不适用，交互启用可授权安装监听组件；尚未就绪返回 requires_action/5，已有 IFEO 不静默停用；查询不会激活保护 |
| 精确进程正常关闭 | 平台能力已实现，独立 review 通过 | 同用户/session/完整身份、限时 WM_CLOSE、1.5 秒等待及显式强制结束，3 项真实隐藏窗口测试通过；实例归属授权、辅助进程收尾及 Guard/CLI 接入待续 |
| Guard 代理参数证据 | 平台能力已实现，独立 review 通过 | 目标主进程的匹配/不匹配/未知，支持 HTTP literal 地址及裸地址等价；其他实例与辅助不提供纠正依据；运行健康、Guard 动作和监听待续 |
| 中文菜单/快捷方式/维护 | 待实现 | 端到端创建启动、改名/绑定/克隆/移除、保护授权、入口修复及保留数据卸载 |
| 发布与实机验收 | 待实现 | 干净环境、Codex/Claude 原版/分身、升级、故障恢复、性能和完整发行清单 |

具体测试矩阵、平台限制和发布门槛仍以 `docs/07-implementation-and-validation.md` 与相关章节为准。用户最近确认的范围优先：不复用外部 sing-box 服务；只创建分身不接管原版；安装路径自动管理；运行中代理故障保留应用；从简实现。

**review / commit 记录**

- IFEO 可行性证据复核：独立审查确认现有 host 重建子进程/DebugDetach 无法恢复 Chromium broker 原有进程/线程句柄和沙箱创建契约。已在第九章记录官方说明、当前源码与本地实现的边界；这不是 Codex/Claude 本机 IFEO 失败实测。暂不开放真实规则，完整要求保留，设计调整待用户答复；先继续独立的订阅功能。提交主题 `docs(rust): record IFEO Chromium continuation limitation`。

- IFEO 只读入口准备：独立 review 通过，提交主题 `feat(rust): validate raw IFEO entry envelopes and context`。固定 debugger 前缀和 UUID 边界，原始 UTF-16 后缀与原生 argv 解析；普通 medium/session/非 sandbox 上下文，受保护规则/deployment 和真实 host/fileID 绑定，仅从登记目标打开 pin，派发前可再次核验。5 项专项独立复跑通过，全量 300 项通过、26 项 ignored，clippy/fmt/diff 通过。此 API 不证明内核触发，也不授予 spawn/continuation；host 模式、coordinator 转交、原始环境和应用级兼容仍待接入。

- IFEO 注册与恢复平台层：独立审查及修复复审通过，提交主题 `feat(rust): persist owned IFEO registry rules and recovery`。注册只接受已登记、绑定代理且启用 Guard 的原版；固定受保护 host 与精确目标身份，跨 store 物理映像归属冲突拒绝。共享父项按 Windows Unicode 大小写规则分组，最后一个参与者解除后才恢复原始 UseFilter；Installing/Removing 仍参与归属。注册表事务在本机返回 6801，采用受保护单值 journal 发布、显式 flush 和逐项核对恢复。修复首记录发布窗口与无记录认领同名 filter 两项审查问题。全量 293 项通过、26 项 ignored；最后补数量上限回归，15 项专项及 clippy/fmt/diff 再次通过。未调用真实 HKLM 写入或 UAC，平台 API 暂不接入生产启用流程。

- Guard 自动扫描与纠正触发：独立审查及增量复审通过，提交主题 `feat(rust): trigger guarded corrections from authorized scans`。合并事件/定时触发、公平有界实例队列、单扫描、授权与配置版本撤销、未知三次短重试后退避；同步持久接纳复用既有精确纠正执行。新增 6 项调度/状态/受控进程及 1 项双向协议测试，全量 280 项通过、26 项 ignored，clippy/fmt/diff 通过；最终提示与调度微调后 12 项监督、2 项 CLI 定向再次通过。修复旧 Ready 覆盖最新未知、秒级并列回执吞掉失败、未决状态覆盖失败诊断。协议 2.10 区分 starting/active/degraded/blocked；真实提权完整事件链路、IFEO 和登录任务待续。本批实际关闭的仅是隔离测试 EXE。

- Guard 普通侧监听监督：独立审查及修复复审通过，提交主题 `feat(rust): supervise authorized guard event listeners`。普通 coordinator 按需核验并启动已有授权任务、认证事件管道、连接恢复及合并补扫通知；服务退出撤销未 dispatch 的准备，超时 native 工作保留名额，结束批次立即降级。新增 6 项测试、全量 273 项通过、26 项 ignored，clippy/fmt/diff 通过；没有真实 RunEx、UAC 或用户应用操作。自动扫描消费者与纠正触发仍待下一批接入，状态不报告 active。

每项记录具体检查、修复结果与 commit；独立 review 没有通过时不提交该功能。实验性代码的 commit 不表示相应生产功能或整批里程碑已经完成。

- M0 基础代码：独立审查 `review_m0` 未发现提交阻塞问题，复跑 7 项测试通过、1 项需 Claude 的测试默认忽略；Claude 包内验证此前单独通过。提交主题 `feat(rust): add reviewed Windows process and package probes`。
- 基础配置模型：独立审查发现并修复 Windows Chromium 参数别名绕过与嵌套未知字段遗漏；复审通过，13 项 core 测试通过。提交主题 `feat(rust): validate typed instance and proxy configuration`。仅配置校验，不表示运行时物理身份、订阅或持久化层已完成。
- 平台配置存储：独立审查发现并修复 ACL 继承标志遗漏与秘密文件写入前权限核验；复审通过，10 项 store 测试通过，clippy 通过。提交主题 `feat(rust): add protected single-owner configuration store`。覆盖文件占用替换失败、坏配置保留、权限变化、junction、秘密引用与版本冲突；不包含恢复界面或运行 journal。
- IPC 传输：独立审查通过，6 项 IPC 测试通过，clippy 通过。提交主题 `feat(rust): authenticate local named-pipe transport`。此批真实管道两端仍在同一测试进程；CLI/host 跨进程验证留给 coordinator 集成测试。
- coordinator 引导：独立审查发现并修复客户端认证失败退出 owner 和空闲计时偏差；复审通过。6 项协议/跨进程测试覆盖竞启、重连、配置保留、短连接、慢客户端和空闲退出。提交主题 `feat(rust): bootstrap one coordinator for status requests`。测试中修复首次初始化竞态和 Windows 后台 host 继承 CLI 输出句柄；仅只读状态功能，不含应用启动/保护。
- 启动模板：独立审查通过，20 项 core 测试（7 项新增模板行为）和 clippy 通过。提交主题 `feat(rust): compile instance launch arguments and environment`。原版清除分身变量，分身生成独立目录参数；已知路径变量只展开一次。目录归属、继承环境总量、代理就绪和实际应用隔离仍需平台验证。
- 实例数据目录：独立审查通过，6 项目录/包命名空间夹具及 2 项真实模板子进程测试通过，clippy 通过。提交主题 `feat(rust): prepare owned isolated instance directories`。默认目录使用 Windows Known Folder；原版零创建、陌生目录拒绝、改名复用、移除保留。包夹具不证明真实 MSIX 可访问，子进程回执不证明 Codex/Claude 隔离或网络代理已经生效。
- 实例配置编辑：独立审查通过，7 项新增行为测试及 core clippy 通过。提交主题 `feat(rust): add instance configuration editing rules`。默认原版；只创建分身不登记原版/IFEO；克隆分配新目录；移除保留数据，已有外部集成时要求先清理。此批仅纯配置规则，不含运行态操作或用户界面。
- 配置请求恢复/去重：独立审查发现并修复完成记录超前于还原后配置的误报；复审通过，8 项事务测试及 workspace clippy 通过。提交主题 `feat(rust): recover durable configuration requests`。全量测试此前 68 项通过，后续修复与新增 2 项回归已在事务套件通过。保护目录内先记录 intent，提交 manifest，再记录回执；不覆盖外部副作用及启动会话恢复。
- 安装解析：独立审查及桥接测试增量复审通过，3 项安装身份/更新夹具与 2 项 PowerShell 桥接夹具通过；真实 Codex/Claude discover 只读查询成功。提交主题 `feat(rust): resolve installation identity and package updates`。本批全量 75 项测试通过，3 项顶层 ignored（其中 2 项由父测试显式调用的子进程 fixture 已运行）；clippy 通过。尚未接入创建/启动，也未验证真实应用启动、多开或代理。
- 配置写服务 / coordinator RPC：独立审查发现并修复旧状态快照覆盖 Guard 空闲策略的竞态；复审及查询恢复增量审查通过。提交主题 `feat(rust): coordinate durable configuration edits over IPC`。全量 81 项测试通过；随后新增临时文件占用查询恢复回归，4 项服务测试通过（共 82 项已验证行为测试）；clippy/fmt 通过。新增管道写测试仍为同测试进程两端；实际 CLI 写链路下一批验证。开发版协议 major 2；尚未实现启动、网络准备或 Guard 执行。
- 实例配置 CLI：独立审查发现并修复长 Unicode locator 超出 IPC 帧限制的分页边界；复审通过。提交主题 `feat(rust): expose instance configuration commands`。4 项真实 CLI→host 测试验证创建/克隆/改名/绑定/移除/查询、只登记分身、安装别名复用及数据保留；列表隐藏参数/环境值，另有字节预算分页回归。全量 87 项测试通过（3 项顶层 ignored 的含义同前），clippy/fmt 通过；不包含真实应用启动、代理或保护执行。
- sing-box 配置生成：独立审查通过，4 项配置契约及 1 项 sing-box 1.14.1 真实进程测试通过。提交主题 `feat(rust): compile shared sing-box proxy configurations`。真实测试覆盖同一内核的双上游认证、未匹配入口拒绝和故障不串线；全量 91 项测试通过、4 项顶层 ignored（其中真实内核测试另行显式运行），clippy/fmt 通过。尚未实现生产内核生命周期、安装或 CONNECT/TLS 健康检查。
- sing-box 程序发现/检查：独立审查发现并修复版本字符串排序错误，跨位数回归及增量复审通过。提交主题 `feat(rust): discover and validate sing-box binaries`。4 项单测、扩展后的真实内核集成、全量 95 项测试及 clippy/fmt 通过。只查找程序并执行 version/check，未实现安装、运行管理或网络健康检查；同步文件 I/O 不受异步超时强制取消。
- HTTPS 代理健康检查：独立审查及真实 sing-box TLS 增量复审通过。提交主题 `feat(rust): verify HTTPS connectivity through explicit proxies`。6 项新行为测试通过，全量 101 项通过（6 项顶层 ignored，其中两个真实内核测试分别显式运行，另三个为父测试调用的 helper）；clippy/fmt 通过。真实 core→HTTP 上游→TLS 夹具验证 204 与 core 退出后失败，不表示生产生命周期、目标应用代理或 Guard 已完成。
- 共享 core 初始生命周期：独立审查及多轮增量复审通过。提交主题 `feat(rust): persist and supervise owned shared core processes`。4 项 generation/journal 契约、1 项原生双栈 PID 归属测试、真实 core 生命周期集成通过；全量 106 项通过，clippy/fmt 通过。修复探测配置竞态、profile 端口映射、恢复失败后丢失共享入口；尚无 coordinator 启动 RPC/启动许可、重配置回滚、无身份启动核对、持续通知及安装。
- core 控制 RPC / CLI：独立审查和增量复审通过。提交主题 `feat(rust): coordinate durable shared core operations`。新增 11 项测试，全量 117 项通过、7 项顶层 ignored，clippy/fmt 通过。覆盖请求规范化、跨操作编号冲突、时钟回拨、终态保留/未决不清理、任务取消、回执占用、并发/丢 ACK 及真实 CLI 跨 host 重启。CLI 接入 start/stop/status/request；未决副作用不自动重放，历史结果与当前监听状态分开。尚无安装、重配置确认/回滚、持续健康通知或应用启动许可。
- sing-box 一键安装 / 取消：独立审查和增量复审通过，提交主题 `feat(rust): install and cancel verified sing-box downloads`。固定官方 1.14.1 artifact，zip/EXE/DLL/LICENSE 校验，protected staging、版本/check、完整目录发布及安装回执；下载/校验移出配置锁而保留 store owner lease。真实受污染代理环境下 CLI 下载、安装、重启查询及复用通过（407.18 秒）；Windows PTY Ctrl+C 持久取消通过。修复长任务占满 16 个 IPC 槽导致查询/取消饥饿，增加独立后台计数和 32 个普通任务上限。全量 126 项通过、9 项顶层 ignored，clippy/fmt 通过；完整协议、应用与 Guard 验收未完成。
- 手动代理配置 / CLI：独立审查与修复复审通过，提交主题 `feat(rust): manage manual proxy profiles and protected credentials`。新增 4 项编辑/认证规则、5 项秘密/事务/运行保护、2 项真实 CLI 测试；全量 137 项通过、9 项顶层 ignored，clippy/fmt 通过，两项真实 sing-box TLS/生命周期集成另行通过。审查发现并修复 pending 配置在旧内核启动后恢复提交的竞态，以及保存阶段遗漏协议认证限制。改名不重启，更新保持 ID/端口，移除检查实例/下载引用；密码独立原子存储，请求回执不含原文。当前活动 generation 编辑返回需重配置，具体影响确认与回滚仍待接入；不表示应用启动或 Guard 完成。

- 共享 core 手动上游重配置：独立审查和增量复审通过，提交主题 `feat(rust): confirm and recover shared core proxy updates`。候选与当前 manifest 分离，确认前检查原程序及配置，具体计划绑定 revision/原 generation/进程；切换期间阻止普通生命周期及配置写入。候选验证修改出口，回滚以旧集合至少一个可用为成功、所有入口核对 PID，最多 4 个并发且每出口获得探测机会。6 项平台 journal、1 项服务恢复、1 项并发探测默认回归新增；全量 145 项通过、11 项顶层 ignored；两项新真实 core/CLI 集成另行通过，clippy/fmt 通过。修复无副作用 busy 请求永久未决、恢复回执摘要未绑定及过期原回执使恢复请求挂起；新增准备回执恢复、计划状态查询。未知 Starting 不盲目重放，活动集合增减/应用启动许可/持续监控未完成。

- 共享 core 代理集合扩容：独立审查、格式修复复审及 CLI 确认入口增量复审通过；提交主题 `feat(rust): confirm and recover shared core profile expansion`。新集合为原集合与本次请求的并集，已存在子集复用同进程；准备不重启，确认绑定精确计划，失败恢复旧集合，扩容不修改 manifest/revision。新增 3 项平台约束/恢复/既有 Rust 记录读取回归和真实 core 扩容测试；真实 CLI 测试同时覆盖编辑及扩容，含默认仅预览、陈旧计划拒绝、显式应用与恢复回执。全量 148 项通过、12 项顶层 ignored；真实扩容、既有更新/回滚和 CLI 三项集成显式通过，clippy/fmt/diff 检查通过。仅兼容本 Rust 上一批 journal/回执，不导入旧 TS 数据。活动集合移除、应用启动许可、完整未知结果核对及持续通知仍待实现。

- 实例启动持久状态 / core 许可：独立审查及增量复审通过，提交主题 `feat(rust): persist launch attempts and protect core startup permissions`。一个受保护原子 journal 保存请求映射、attempt 和确认会话；同实例未决请求指向同一 attempt，跨配置/core 请求编号冲突拒绝。创建前必须成功持久化 intent，旧 epoch 的前置准备可结束，SpawnRequested 之后保留未知且不按时间释放；晚取消不声称未启动。ReadyToSpawn 发布与 core stop/reconfigure 共用 gate，持久许可在未知结果期间继续阻止破坏性变更。新增 7 项平台行为测试（含真实子进程）及扩展的真实 core 竞争测试通过；workspace 154 项通过后新增容量/时间回归单独通过，共 155 项默认行为已验证、13 项顶层 ignored，clippy/fmt/diff 通过。尚未接实际 LaunchEngine、CLI、跨 store 资源锁、MSIX 回执或确认未创建时的失败终结，不等于生产启动闭环已完成。

- 跨 store 实例预留 / 一次性执行许可：独立审查与修复复审通过，提交主题 `feat(rust): reserve physical instances across stores before dispatch`。按物理安装/数据身份生成 key，独立用户目录内核文件锁配合持久占用，owner 死亡不清除未知；确认后仅精确退出可释放。审查发现并修复未创建证据可重用和跨 store 混用：本地原子写后唯一发放带随机 nonce 的许可，全局授权消费许可，平台创建再消费授权，证据完整绑定 store/attempt/epoch/nonce。新增 7 项行为测试含真实跨进程锁及创建/退出；全量 162 项通过、14 项顶层 ignored，真实 core 扩容/许可竞争另行通过（7.81 秒），clippy/fmt/diff 通过。尚未接入生产 LaunchEngine/CLI、外部进程发现和 MSIX helper，不表示完整启动已交付。

- 进程只读查询：独立审查通过，提交主题 `feat(rust): inspect exact process arguments with bounded native WMI`。ToolHelp 提供有界快照提示，独立 COM 线程读取参数并复核完整身份/创建时间；查询无终止权限，未提供命令行不等于空参数。5 秒调用预算与一个实际未结束查询的限制覆盖超时、取消和异常；参数及 provider 描述不外传。5 项新增测试含真实子进程参数回执和身份伪造拒绝，审查方独立复跑通过；全量 167 项通过、15 项顶层 ignored，clippy/fmt/diff 通过。尚未提供实例归属分类、完整 LaunchEngine 或 Guard 执行。

- 单进程实例归属：独立审查、目录访问边界及尾点路径修复复审通过，提交主题 `feat(rust): attribute exact processes to physical instance data`。匹配物理映像及数据目录，分开主进程/辅助进程与目标/其他/未知；没有数据目录参数的原版不会被只管理分身的目标认领。重复/不可读/相对参数、未核对旧包和辅助进程缺少目录保持未知。只读目录比较与 WMI 共用预算和原生句柄；拒绝远端、设备路径和逐级 reparse，不因外部进程参数发起远端访问。保留 verbatim 语义，修复尾点目录被错误规范化为另一目录的误判。全量 171 项通过、16 项顶层 ignored；最终尾点回归及 4 项归属测试再通过，审查方独立验证；clippy/fmt/diff 通过。本批不是全局空闲证明，尚未接实际启动或 Guard 动作。

- 普通 EXE LaunchEngine 服务：独立审查及恢复修复复审通过，提交主题 `feat(rust): execute and recover ordinary application launches`。共享 core manager 准备代理、物理占用与配置摘要复核、一次性创建、独立后台任务、持久取消、失败不直连；外部进程只拒绝重复启动，不接管或终止。审查修复待提交配置未恢复、双 journal 确认/释放中断、未 ACK 证据被覆盖、未同步记录过期及无关恢复阻断历史重放；同 store 持锁 Reserved 可在旧准备记录过期后回收，旧 dispatch 不能用于新 owner。新增 12 项 engine 行为测试、1 项 IFEO 只读检查、1 项未同步保留与 1 项旧许可拒绝；全量 184 项通过后最终 2 项回归及对应 12/8 项套件通过，共 186 项默认行为已验证、17 项顶层 ignored；真实 sing-box 扩容/core 许可回归另行通过，clippy/fmt/diff 通过。创建的是隔离测试 EXE，未启动/停止用户 Codex/Claude，未写 HKLM IFEO；RPC/CLI、MSIX、IFEO continuation、运行监控及真实应用验收仍待实现。

- coordinator 普通 EXE 启动 RPC：独立审查及历史会话修复复审通过，提交主题 `feat(rust): coordinate application launch requests over IPC`。协议 2.7 提供启动/查询/取消，启动与 core 控制共用同一 CoreManager；接纳先于 ACK，长任务不占连接槽。完成通知和 5 秒只读维护核对精确退出，未知保活。新增 3 项真实管道测试覆盖丢 ACK、重复/冲突、等待期间查询取消、应用存活及退出后 owner 空闲退出、旧 minor 拒绝；全量 189 项通过。随后修复历史登录会话被当前 session 限制阻断恢复，新增 1 项合成历史会话回归及 8 项启动状态/3 项管道回归通过，共 190 项默认行为已验证、17 项顶层 ignored，clippy/fmt/diff 通过。只读历史恢复仍要求同 SID/完整身份与创建时间，创建/终止/普通预留不放宽；没有实测切换 Windows 登录会话。CLI/菜单、MSIX、IFEO 转交、持续网络监控和真实应用验收待续。

- 普通 EXE 启动 CLI：独立审查与取消/版本竞态修复复审通过，提交主题 `feat(rust): launch instances with inline dependency repair`。接入 launch/inspect/cancel、缺失内核安装、共享代理扩容影响确认；JSON/非交互返回待操作信息，终态重放不修复或重新创建。前台 Ctrl+C 意图跨提示/准备/应用持续保留，自动继续的 revision 在服务端接纳及最终派发均核对，协议 2.8 拒绝旧 host 忽略前提。新增 2 项真实 CLI 与 1 项 engine 竞争回归，旧协议覆盖扩展；全量 193 项通过、18 项顶层 ignored，真实 sing-box CLI 集成与 Windows PTY 提示取消另行通过，clippy/fmt/diff 通过。审查方独立复跑 CLI、revision 和协议测试通过。未实测成功内核切换中的 Ctrl+C，MSIX、IFEO/Guard、中文菜单及真实应用完整验收仍待实现。

- MSIX 一次性包请求与撤销平台：独立审查与三项边界修复复审通过，提交主题 `feat(rust): persist one-use package launch capabilities`。消费现有 dispatch/global permit，完整 staging 原子发布、同一 gate 消费/撤销、nonce/原 journal/目录身份绑定；创建后包身份或回执失败保持未知。修复 UTC 回拨延寿、部分发布无法核对及遗漏 child 包身份，发行 tick 与原发行者精确存活共同限制授权。新增 8 项默认行为测试含实际文件占用与普通子进程，全量 201 项通过、19 项顶层 ignored；审查方独立复跑 8 项通过，clippy/fmt/diff 通过。新测试的包上下文为私有注入，真实包 helper/应用及 bridge/host/LaunchEngine 接入仍待完成，不能以此代替 MSIX 生产启动验收。

- MSIX 生产启动接入及恢复：独立审查与增量复审通过，提交主题 `feat(rust): integrate package launches and durable recovery`。bridge 激活 package-child，LaunchEngine 消费一次性能力并等待持久回执；取消/截止由同一 gate 撤销，未知保留应用。隔离存储包将请求发布至自有 LocalState/store UUID 命名空间，helper 不访问被包虚拟化的原 store。修复旧包映像候选遗漏；同 basename 的未核对旧版本保守阻止重复创建。新增 namespace、旧包候选、四种恢复状态和跨 store 同 attempt 回执拒绝 4 项默认测试，全量 205 项通过、20 项顶层 ignored；真实 Claude 过期 helper 集成另行通过（24.13 秒），不启动 Claude 应用。审查方独立复跑相关 15 项通过；clippy/fmt/diff 通过。真实应用分身启动、数据隔离、代理及 Guard/IFEO 验收仍待续。
- 辅助进程存活祖先归属：真实 Codex 原版辅助进程无数据目录参数导致分身误拒绝，现通过同映像/SID/session/更早创建时间的存活祖先补充只读归属，最多 8 层共享查询预算，每层句柄返回时复核；无终止授权。独立 review 通过，审查方复跑 5 项归属及 6 项查询测试通过。新增真实父子进程回归，全量 206 项通过、20 项顶层 ignored，clippy/fmt/diff 通过。实际 Claude/Codex 各两个直连分身同开、独立目录写入、重复启动复用通过；Codex 原版一直存活。测试进程通过核对身份后的自有句柄清理，四个 attempt 均核对退出。提交主题 `fix(rust): resolve auxiliary ancestry before instance launch`。这些实机结果不等于登录、代理、Guard/IFEO 或完整发行验收。
- 精确进程正常关闭：独立 review 通过，提交主题 `feat(rust): close exact application processes with bounded waits`。先以查询权限核对身份并限时请求目标窗口关闭，等待 1.5 秒；仅调用方显式选择 force 才申请终止权限并再次核对，终止后最多等待 3 秒。单次消息最多 100ms、消息总预算 1 秒，窗口消息返回不构成退出证据。新增 3 项真实隐藏窗口测试，全量 209 项通过、21 项顶层 ignored；最终等待时间修订后 3 项定向复跑通过，审查方独立复跑通过，clippy/fmt/diff 通过。仅提供单进程能力，不按名/树终止，不代表实例停止、辅助清理或 Guard/IFEO 已完成。
- Guard 代理参数证据：独立 review 和误判修复复审通过，提交主题 `feat(rust): inspect proxy arguments on exact managed main processes`。复用身份/目录/角色观测，仅目标 Main 提供 Matching/Mismatched/Unknown；辅助和其他实例不能成为纠正目标。复审修复 Chromium 裸 IP:port 与 HTTP URI 等价写法；命名 host、复杂映射/回退列表和未知 scheme 保留 Unknown。新增 1 项参数回归并扩展真实 child 契约，全量 210 项通过、21 项顶层 ignored，修复后 6 项归属测试和 clippy/fmt/diff 通过，审查方独立复跑通过。没有健康探测或停止动作，运行故障不得据此关闭应用；Guard 的会话绑定选择、限流、监听和执行集成待续。
- Guard 纠正执行服务：独立 review 及增量复审通过，提交主题 `feat(rust): execute guarded instance correction with durable stop intent`。新增内部入口、一次性持久停止授权、跨 store 物理限流、精确关闭和辅助残留等待；代理失败不直连。停止前后取消/配置变更、迟到 worker、回执丢失恢复均有真实夹具回归。全量 219 项通过、21 项顶层 ignored；最终内存布局与测试锁作用域修订后 clippy 和 Guard 定向测试通过。尚未启用自动扫描/ETW/IFEO，无用户应用被关闭。

- Guard 只读实例扫描：独立 review 及增量复审通过，提交主题 `feat(rust): scan guarded instances without disturbing managed sessions`。扫描保留 Confirmed 的历史网络绑定，排除未管理原版，未决/身份未知/多个主进程不输出纠正目标；完成后再次检查配置 revision 和新未决请求。新增只打开既有分身目录的接口，不创建或认领 store/LocalState 数据。独立复跑 6 项扫描和 2 项目录测试通过；全量 227 项通过、21 项顶层 ignored，clippy/fmt/diff 通过。解析工作线程持有单槽直到实际返回，5 秒扫描超时不会堆积后台解析。尚未接自动监听、授权、ETW 或 IFEO。

- Guard CLI 与授权状态边界：独立 review 及 JSON 严格性修复复审通过，提交主题 `feat(rust): expose guard configuration and authorization status`。新增 status/enable/disable、2.9 RPC、单槽观察及旧 host 拒绝；保留配置回执，组件未授权返回 requires_action/5，现有 IFEO 不允许静默停用。只分身 IFEO 不适用、manifest 登记不视为实际 active。新增 5 项回归，全量 232 项通过、21 项 ignored；输出调整后 10 项 CLI 契约再次通过，独立复跑 17 项 Guard 测试通过，clippy/fmt/diff 通过。没有安装特权组件或自动监听/纠正。

- 原生 ETW 进程事件平台层：独立审查及修复复审通过，提交主题 `feat(rust): add bounded native process event listener`。固定 Microsoft-Windows-Kernel-Process / ProcessStart，TDH 按属性名解码，事件只提供 PID/短名/时间提示；队列去重、溢出和解码/系统丢失要求补扫。原会话 handle、GUID 和名称核对后才停止，不接管同名会话。修复会话已结束时 drain 丢失最终批次的问题，保留真实结束原因。全量 236 项通过、24 项顶层 ignored，clippy/fmt/diff 通过；真实空会话冲突/归属/外部结束测试另行通过。当前普通令牌启用 kernel provider 返回 Win32 5，不能宣称真实事件采集通过；检查无残留本产品 ETW session。提权部署、事件 IPC 和自动 Guard 尚未接入。

- 单向事件管道平台层：独立审查通过，提交主题 feat(rust): authenticate one-way privileged event streams。提权发送/普通只读、独立名称和 logon ACL、完整对端身份/权限、严格有界消息及重连/丢失补扫；事件每批 128 条且全部队列送完才报告结束。5 项管道与新增 1 项队列结束回归通过，审查方复跑相关 10 项通过。全量 242 项通过、24 项 ignored，clippy/fmt/diff 通过。真实管道使用普通 token 私有夹具，真实提权两端、部署与自动 Guard 尚未验收。

- Guard 受保护 helper generation：独立审查与来源绑定修复复审通过，提交主题 feat(rust): stage verified administrator-owned guard helpers。固定自动目录、UAC 前 host/父目录 pin 和独立 fileID/size/hash、同用户普通 issuer 校验、管理员 owner/Users RX ACL、独占代际文件和回读；失败不激活或清除未知对象。6 项默认回归、全量 248 项通过，24 项 ignored，clippy/fmt/diff 通过。没有真实提权目录写入；前台必须保留 InstallerSource 并按固定 UAC 参数传递期望，任务/监听激活仍待接入与实测。

- Guard 按需监听任务平台层：独立审查与增量复审通过，提交主题 feat(rust): validate and manage fixed guard listener tasks。固定 task/action 与 UUID 参数、管理员 owner/用户读运行 ACL、SID 解析、V2/context/触发和生命周期核验、当前 session RunEx 与空闲删除回读；不覆盖未知任务、不强制停机。4 项默认新增测试通过，全量 252 项通过、24 项 ignored；后续 task 收紧和 maintenance 变异回归定向再通过，clippy/fmt/diff 通过。本机仅建立内存 COM definitions 和只读查询，没有注册/运行/删除任务或 UAC；event-listen/前台授权/登录任务仍待接入。

- Guard 生产监听入口与崩溃恢复：独立审查及增量复审通过，提交主题 `feat(rust): run protected guard listeners with trace recovery`。host 接入固定 event-listen/store/generation，核验提升权限和受保护自身/普通 coordinator 映像；统一认证截止后才开启 ETW，心跳/完整排队结束/断开停机。受保护 store/session journal 跨 generation 独占写入，先持久化 epoch，恢复按旧 epoch 及查询返回 handle 二次核验，冲突保留。7 项新增平台测试、真实 host 参数及普通权限拒绝契约通过；全量 260 项通过、25 项 ignored，新增真实空 ETW 恢复项单独显式通过；clippy/fmt/diff 通过且无测试 trace 残留。尚未连接前台授权、普通侧监听监督和自动 Guard，不宣称保护已激活；没有 UAC、任务注册或用户应用操作。

- Guard 前台监听组件授权安装：独立审查及修复复审通过，提交主题 `feat(rust): authorize guard listener installation from foreground`。交互 guard enable 内询问、固定 runas/hex ticket/源文件 pins 与 issuer、受保护安装锁/不可变 generation 意图先于注册、结果未知保留/精确部署复用、普通回读任务验证；JSON/非交互/后台不提权。修复旧扫描与核验串行超出 RPC 期限，改共享 3 秒绝对截止，慢双观察真实 pipe 回归通过。新增 installer/intent/host/RPC 7 项默认回归；全量 267 项通过、26 项 ignored，clippy/fmt/diff 通过；Windows PTY 暂不安装和提示阶段 Ctrl+C 通过，自有测试 coordinator 已退出。真实 UAC 期间取消、提权目录/任务写入及注册未知实机恢复仍未验证；监听监督、登录任务、自动 Guard、IFEO 与完整产品验收待续。
