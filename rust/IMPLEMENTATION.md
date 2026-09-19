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
| 实例/模板/启动 | 待实现 | 原版与空白实例、受管参数环境、实体操作、统一状态机和已运行判断 |
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
