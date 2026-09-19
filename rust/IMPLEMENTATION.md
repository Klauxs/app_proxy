**实现与验收跟踪**

目标：按已确认设计完整实现 Rust 版，每项功能经独立 review、修复与验证后单独 commit。此文件记录进度，不缩小设计范围；不把实验、编译或局部测试作为整个产品完成的证据。

| 功能 | 当前状态 | 完成证据 / 下一项验证 |
|---|---|---|
| M0 普通进程/身份/调试创建候选 | 已实现，独立 review 通过 | `TEST-RESULTS.md`，普通/调试 child 契约测试；不等于完整 M0 通过 |
| MSIX 包内 helper | 仅实验通过 | Claude 包内回执；生产启动、取消/迟到、应用实例仍待实现 |
| 配置模型 | 基础模型已实现，独立 review 通过 | 严格 JSON、引用/路径/Guard/IFEO 校验；13 项 core 测试；当前节点仅手动 HTTP/SOCKS5，订阅与其他协议后续实现 |
| 配置存储 | 基础存储已实现，独立 review 通过 | 归属/ACL、revision、锁、原子替换/上一份备份、不可变秘密与坏文件拒绝；10 项 store 测试；恢复界面与运行 journal 待实现 |
| IPC 传输 | 已实现，独立 review 通过 | 双向身份/普通权限/会话/映像验证、管道 ACL、1 MiB 帧与超时/取消；4 项真实管道 + 2 项帧测试 |
| coordinator | 引导功能已实现，独立 review 通过 | status、单所有者竞启、协议握手、慢客户端隔离、重连、30 秒空闲退出；写请求去重/journal/实际任务恢复待实现 |
| 启动模板 | 纯合成已实现，独立 review 通过 | 原版/分身参数环境、直连/代理、秘密引用、路径变量；7 项模板测试；不表示代理就绪或可以启动 |
| 实例数据目录 | 已实现，独立 review 通过 | store/LocalState 命名空间、归属/ACL/目录句柄、空白创建、保留数据；6 项目录测试 + 2 项模板子进程测试；真实包内访问待验证 |
| 实例配置编辑 | 已实现，独立 review 通过 | 创建/克隆/改名/绑定/移除的纯配置规则，7 项测试；coordinator 写接口、物理安装去重和外部集成清理待接入 |
| 配置请求恢复/去重 | 已实现，独立 review 通过 | 8 项事务测试覆盖 pending 两侧恢复、结果重放、文件占用、损坏及配置回退；仅纯配置事务，待接入 IPC |
| 实例启动 | 待实现 | 统一状态机、安装解析和已运行判断 |
| 安装解析 | 已实现，独立 review 通过 | EXE 文件身份/别名、短期句柄、MSIX 稳定定位/旧计划拒绝；3 项安装 + 2 项桥接夹具测试，真实 Codex/Claude 只读发现通过；待接入登记及启动 |
| sing-box 管理/一键安装 | 待实现 | 自行启动、多个入口/出口共享进程、安装/取消、CONNECT/TLS、配置切换恢复 |
| 订阅解析 | 待实现 | 六协议、URI/Base64/Clash/文本格式、兼容 fixtures、刷新保持选择 |
| IFEO | 仅调试创建候选 | 实际注册匹配/防递归/子进程/调用语义/恢复；未管理原版不受干预 |
| Guard/ETW | 待实现 | 默认范围、普通/提权分工、事件/扫描、身份未知、限流及未管理进程存活 |
| 中文菜单/快捷方式/维护 | 待实现 | 端到端创建启动、改名/绑定/克隆/移除、保护授权、入口修复及保留数据卸载 |
| 发布与实机验收 | 待实现 | 干净环境、Codex/Claude 原版/分身、升级、故障恢复、性能和完整发行清单 |

具体测试矩阵、平台限制和发布门槛仍以 `docs/07-implementation-and-validation.md` 与相关章节为准。用户最近确认的范围优先：不复用外部 sing-box 服务；只创建分身不接管原版；安装路径自动管理；运行中代理故障保留应用；从简实现。

**review / commit 记录**

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
