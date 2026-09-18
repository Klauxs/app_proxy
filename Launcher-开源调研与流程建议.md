**PortableApps Launcher / Prism Launcher：对 App Proxy 的流程借鉴**

调研日期：2026-09-18。结论：优先借鉴 PortableApps 的应用适配配置和 Prism 的实例管理、启动任务模型，保留当前 TypeScript / PowerShell 实现。最先值得落地的是启动诊断、Guard 失败策略和实例创建流程；完整配置继承、通用脚本扩展可以后置。

**调研范围与证据**

本次阅读了官方文档，并将两个上游仓库浅克隆到系统临时目录，检查了启动、实例、环境变量、失败清理等源码。上游引用固定到以下提交，避免主分支变化后无法复核：

| 对象 | 本次源码基线 | 说明 |
|---|---|---|
| PortableApps Launcher | `5470210a6841c38950c348e1190ad92787b8b727` | 该仓库 HEAD 提交日期为 2024-04-16；不能据此推断整个 PortableApps 项目停止维护 |
| Prism Launcher | `d06c8a742894229d4967fb0bf16b9bf40cd377d3` | 该仓库 HEAD 提交日期为 2026-09-18；这里讨论所读源码，不等同于所有发行版行为 |
| App Proxy | `D:\app_proxy`，`main`；开始读取时 HEAD `140ac9fa871276ae58f6e1b2f8f451f013dac2d8` | 以读取时工作区代码为准；本次未修改产品代码 |

交付前复核时，其他工作已将 HEAD 更新为 `5d42d65172a8e1acd121792cc4753b3f33724156`（Guard ETW 通知）。比较上述两个提交，本文主要对照的 applications、guard、types、msix、cli、store、service、core 和 integration 源文件没有变化；本文结论仍适用。本次仅新增本调研文档。

当前流程以 Windows 实现为主要对照，macOS 仅参考已有说明。没有运行上游应用、编译项目或重新执行本地实机测试；风险判断属于源码静态分析。本文中的建议流程和类型都是设计草案，不代表已经实现。

**1. 两个项目分别解决什么问题**

PortableApps Launcher 的核心是“给一个已有应用补齐启动环境”。它从配置读取目标程序、工作目录、参数和环境变量，再由不同 segment 在准备、执行前、执行后和清理阶段工作。应用差异可以通过配置或少量自定义代码表达。[启动配置定义](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/Manual/ref/launcher.ini/launch.rst)，[segment 机制](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/Segments.nsh)。

Prism 的核心是“管理很多独立实例，并负责每个实例的启动任务”。实例包含根目录、设置、名称、图标、状态和启动过程；部分设置可以继承全局值。它的实例仍然是 Minecraft 专用模型，需要提炼概念，不能直接当通用桌面 launcher 后端。[BaseInstance](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/BaseInstance.cpp)。

对 App Proxy 的映射是：应用模板描述“怎么启动”，实例描述“这次使用哪份数据和代理”，启动任务负责“检查、准备、执行和确认”。目前 `App` 同时承载安装位置、实例配置和应用类型，`instance: 'codex' | 'claude'` 同时表达品牌与分身模式，这使后续扩展容易继续增加条件分支。[当前类型](D:/app_proxy/windows/src/types.ts:7)。

**2. PortableApps 最值得借鉴的三点**

**声明式适配能减少创建实例时的技术选项。** `[Environment]` 可以表达环境变量及路径替换，`[Launch]` 表达程序、参数和工作目录。它把这些知识放在应用包装配置里。[环境变量文档](https://portableapps.com/manuals/PortableApps.comLauncher/ref/launcher.ini/environment.html)，[Environment 实现](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/Segments/Environment.nsh)。

当前菜单让用户输入 EXE、选择 Codex/Claude 分身、确认 Chromium 参数支持，再输入参数 JSON；应用知识分散在用户输入和 `applications.ts` 分支里。建议先做内置模板表，集中描述代理参数能力、实例目录参数、需设置或移除的环境变量、进程匹配规则及已验证范围。包识别和 MSIX 传输继续由通用平台代码完成。[当前创建菜单](D:/app_proxy/windows/src/cli.ts:160)，[当前实例环境生成](D:/app_proxy/windows/src/applications.ts:106)。

建议先支持现有 Codex、Claude、通用环境变量应用三个模板；通用 Chromium 只声明代理启动能力，独立实例能力仍需明确适配。不能因为某应用使用 Electron 就宣称它的所有组件支持分身。模板识别应该依赖包身份、程序元数据或已验证规则，不只看文件名。

**生命周期阶段值得引入。** PAL 将初始化、准备、执行前、执行后、收尾分别安排，并区分主启动器与重复启动器；重复启动不应再次执行同一套设置搬运。它还有运行期记录及 Starting/Stopping mutex，发现上次异常遗留时进入清理分支，而不是直接当成一次全新启动。[启动与恢复入口](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/PortableApps.comLauncher.nsi#L387)。

对当前项目，借鉴点是显式区分“启动中、回执待确认、已确认进程、已退出、失败”，让重复点击和恢复走同一套规则。现有 MSIX pending 请求和有效期机制已经做对了一部分，可以推广为所有启动方式共用的 attempt 状态，而不是重写这套保护。[MSIX 回执](D:/app_proxy/windows/src/msix.ts:11)。

**程序、数据与展示名称分离值得保留。** PAL 使用独立的 App、Data 和默认数据区域，首次初始化有明确入口。[Settings segment](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/Segments/Settings.nsh)。当前项目已经按 UUID 分配实例目录，快捷方式也按 ID 启动；改显示名称不搬数据的方向是正确的。[实例目录](D:/app_proxy/windows/src/applications.ts:33)，[快捷方式](D:/app_proxy/windows/src/integration.ts:17)。

不建议照搬 PAL 的目录搬入/搬出及注册表备份恢复机制。它的 `DirectoriesMove` 会在启动前备份原位置并移入便携数据，结束后搬回。这里存在共享位置和退出清理依赖，不适合当前原版、多个分身同时运行的目标。继续通过参数和进程环境指定独立目录。[DirectoriesMove 源码](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/Segments/DirectoriesMove.nsh)。

PAL 的部分单实例判断按可执行文件名查进程，也不应替换当前包含路径、创建时间、用户、会话和实例目录的身份校验。[InstanceManagement](https://github.com/PortableApps/Launcher/blob/5470210a6841c38950c348e1190ad92787b8b727/Other/Source/Segments/InstanceManagement.nsh)。

**3. Prism 最值得借鉴的四点**

**启动任务是有状态的步骤序列。** `MinecraftInstance::createLaunchTask()` 组装目录准备、依赖检查、命令、更新和进程启动等步骤；`LaunchTask` 按结果推进，失败停止，正常完成或步骤失败时对已进入的步骤逆序调用 `finalize()`。任务也能报告进度及其是否允许取消。[任务组装](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/minecraft/MinecraftInstance.cpp#L1136)，[任务执行](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/launch/LaunchTask.cpp#L95)。

当前 `launchUnlocked()` 已经有正确的主要步骤顺序，但异常只向外冒泡，用户不易知道卡在哪一层；普通 EXE 启动后也只做一次 PID 身份查询。建议增加统一阶段及结果类型，而不是先引入复杂任务框架。[当前启动实现](D:/app_proxy/windows/src/applications.ts:92)。

特别注意：Prism 的 `PostExitCommand` 是排在游戏进程步骤之后的普通步骤。所读代码中，游戏崩溃或非零退出会令任务失败，后置命令不保证执行；真正的资源释放入口是 `finalize()`。App Proxy 应分别定义“成功之后的扩展动作”和“所有终止路径都必须执行的清理”。逆序清理也不等于完整事务回滚。[进程结束处理](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/minecraft/launch/LauncherPartLaunch.cpp#L161)。

**实例复制应该是有选择的操作。** Prism 允许选择复制哪些数据，并在创建副本后生成新的 UUID；实例创建先进入临时目录，成功后提交，部分失败和取消路径清理临时目录，提交受文件占用影响时有退避重试。[复制选项](https://prismlauncher.org/wiki/help-pages/instance-copy/)，[复制完成逻辑](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/InstanceCopyTask.cpp#L141)，[暂存与提交](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/InstanceList.cpp#L959)。

适合当前项目的第一步是“复制启动配置并创建空白实例”：沿用安装来源、适配类型、可选参数和代理绑定，生成新 ID、新目录，Guard 默认关闭。默认不复制登录状态、数据库、Cookie 或凭据，也不照搬硬链接/符号链接共享可写数据。后续若支持数据复制，应单独设计并要求源实例退出。当前目录安全策略还会拒绝符号链接，直接引入链接克隆会与它冲突。

**继承必须有显式开关。** Prism 的 `OverrideSetting` 根据 gate 决定读实例值还是全局值；Minecraft 实例的环境变量也通过 OverrideEnv 接入继承。它是明确的继承/覆盖选择，不是任意深度的 JSON 合并。[OverrideSetting](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/settings/OverrideSetting.cpp)，[环境配置注册](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/minecraft/MinecraftInstance.cpp#L240)。

目前没有必要立即引入完整继承系统。如果以后支持默认代理，必须区分 `inherit`、`direct`、`profile(id)`；当前无 `profileId` 的含义是直连，迁移后必须保持，不能悄悄解释成继承默认代理。界面应显示最终值及来源，例如“代理：美国出口，来自实例设置”。

**受控环境是一个独立步骤。** Prism 先清理已知影响 Java 的父进程环境，再注入实例变量和自定义环境。值得借鉴这种构造顺序，不能照搬它的 Java 变量黑名单。[环境清理](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/java/JavaUtils.cpp#L79)，[环境组合](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/minecraft/MinecraftInstance.cpp#L680)。

当前 `childEnvironment()` 已经清除继承的代理变量，分身也会重设相应目录变量。这里不需要从头做；需要将已有规则集中，并让普通 EXE 和 MSIX 使用同一个经过校验的环境变更描述。MSIX 目前只发送七个固定变量，新增模板里的变量否则会在普通 EXE 生效、到 MSIX 静默丢失。不要直接把 launcher 全量环境写入请求文件，应只传递经允许的 set/unset 变更；包内基础环境仍由包上下文提供。[普通环境构造](D:/app_proxy/windows/src/applications.ts:13)，[MSIX 环境白名单](D:/app_proxy/windows/src/msix.ts:26)，[包内应用环境](D:/app_proxy/windows/src/msix-child.ts:14)。

**4. 对当前流程的具体改进**

| 环节 | 当前情况 | 建议行为 | 优先级 |
|---|---|---|---|
| 添加应用 | 名称、EXE、分身品牌、适配器、JSON 参数逐项输入 | 选择安装应用 → 原版或空白实例 → 名称与代理 → 创建摘要；参数放高级选项 | P1 |
| 代理选择 | 无记录时先自动发现服务，然后才能选直连 | 先选直连、已有配置或添加代理；选择代理路径时才触发发现 | P1 |
| 创建完成 | 登记、快捷方式、Guard 依次执行，后一步失败可能留下已创建记录 | 返回实例已创建及各附加步骤结果；提供继续创建快捷方式/启用保护，避免用户重新登记 | P1 |
| 启动 | 整段逻辑包在全局锁内，最后返回 PID 与说明 | 先增加阶段输出、attempt ID、失败原因和耗时；后续再拆锁 | P0 |
| 启动状态 | 保存最近启动身份，status 展示 lastPid | 实时验证记录身份；区分运行、已退出、待确认、失败，不把历史 PID 当当前状态 | P0 |
| Guard | 关闭进程后调用启动流程，之后才可能发现代理不可用 | 明确断网策略；替换前检查依赖；关闭与重启分别报告结果 | P0 |
| 第二个实例 | 重新选择 EXE、品牌和参数 | 从现有配置创建空白副本，仅改名称和代理；新身份、新目录 | P1 |
| 更换安装位置 | MSIX 按包身份解析；普通 EXE 不提供专门重定位操作 | 增加重新定位应用，保留实例 ID、数据和代理绑定，验证后提交 | P2 |
| 工具目录升级 | 快捷方式和任务绑定当前工具文件路径 | 采用固定安装入口或显式入口修复流程，升级时保持数据根与实例身份 | P2 |

P0 表示对既有流程可靠性直接有帮助，不表示已完成运行时故障复现。P1 是主要使用体验改善。P2 需要额外的迁移和平台设计。

添加流程最值得去掉的是“用户先判断技术适配类型”。一个实际交互可以是：

```text
添加实例
  应用：Claude（已识别安装来源）
  数据：使用原有数据 / 创建空白实例
  名称：Claude - 工作
  网络：直连 / 选择代理 / 添加代理
  高级：启动参数、工作目录

创建后
  实例已创建
  桌面快捷方式：已创建 / 未创建，可重试
  保护：关闭 / 已启用 / 未完成授权
  操作：启动 / 返回实例列表
```

代理发现和联网探测可能较慢，但本次没有测量延迟；改善顺序的理由是避免用户选择直连时仍执行不必要的网络前置流程。自动列出安装应用属于新增能力；第一版仍可保留粘贴 EXE，再做识别。

**5. 启动流程建议：共用准备结果，保留平台适配**

建议先形成一个内存中的 `LaunchPlan`，包含本次解析后的 EXE/包身份、工作目录、参数数组、环境变更、实例目录、代理绑定快照和身份匹配规则。完整环境和参数不写普通日志；面向用户只输出经过脱敏的摘要。计划关联配置版本，防止检查期间配置变化后按旧结果执行。

```text
读取配置与建立启动意图
  → 解析当前安装 / MSIX 包及数据位置
  → 校验参数、目录、模板能力和重复启动
  → 准备并验证指定代理
  → 构造 LaunchPlan
  → 复核实例和配置仍匹配
  → 普通 EXE 或 MSIX 执行器启动
  → 确认回执与实例身份
  → 记录本次结果
```

菜单、桌面快捷方式、CLI、Guard 都使用这条生产启动链。普通 EXE 与 MSIX 是同一条链上的执行器差异；Guard 负责发现和决策，然后复用启动链。现有项目已经共用了 `Applications.launchUnlocked()`，应在这个基础上拆分阶段。

运行结果需要把三个维度分别记录：启动尝试的状态、已确认的进程状态、网络证据等级。代理探测成功和进程创建成功不能合并成“目标应用所有流量已代理”。建议对外表达“代理入口验证通过；进程已确认；应用流量未验证”，保持当前已有的证据边界。[现有证据说明](D:/app_proxy/windows/src/service.ts:76)。

普通 EXE 创建后立即退出可能是转交其他进程，也可能是真的失败。可以做有时限的身份确认，但不能仅凭同名进程接管成功记录，也不能在 MSIX 回执未定时盲目重试。进程记录失效后应标记已退出或未知，不凭旧 PID 终止进程。

清理只处理本次尝试拥有的临时请求、预留记录等资源。实例数据默认保留；外部 sing-box 不归工具所有；托管 sing-box 可能被多个实例共享，单个实例退出或失败不能直接停止它。当前 `Core.applyUnlocked()` 已有配置检查、备份和失败恢复，不必借调研重写代理配置层。[现有内核配置恢复](D:/app_proxy/windows/src/core.ts:128)。

**Guard 的策略必须先说明白。** 当前路径是 `stop(target) → 检查剩余进程 → launchUnlocked()`；后者才探测代理。因此代理不可用时存在“已关闭但未重启”的路径。[Guard 顺序](D:/app_proxy/windows/src/guard.ts:114)，[代理前置检查所在位置](D:/app_proxy/windows/src/applications.ts:98)。

建议抽出不启动目标应用的 preflight。明确两种不同策略：若保护目标是尽快阻止检测到的直连，应允许关闭后进入“因代理不可用而阻止重启”，并准确告知；若用户明确选择“仅在可重启时替换”，才在 preflight 通过后关闭，失败时保留原进程。这后一策略会保留直连窗口，不能作为无声的行为变更。即使 preflight 成功，也不能保证关闭后网络不再变化，真正启动仍需复核。当前 Guard 本身是事后纠正，不构成网络层强制阻断。

**锁粒度可以改进，但应该单独做。** `Applications.launch()` 持有全局配置锁，期间包含内核启动、网卡探测、代理请求以及 MSIX 回执等待。源码上能够判断其他操作会等待这把锁，不能在未测量时量化耗时。[启动入口](D:/app_proxy/windows/src/applications.ts:91)，[配置锁](D:/app_proxy/windows/src/store.ts:109)。

后续可采用实例级启动预留、共享内核操作锁和短时 manifest/runtime 提交锁。读取快照 → 锁外耗时检查 → 锁内复核与提交时，还要保留跨进程重复启动保护、共享内核串行化和配置变化校验。仅把 `store.lock()` 去掉会引入竞态。这不是 Prism 现成提供的 Windows 解决方案，而是对当前代码的工程推导。

**6. 最小实现顺序与验收条件**

| 批次 | 范围 | 验收重点 |
|---|---|---|
| 第一批：启动可诊断 | 将现有方法提取为有限阶段；attempt 结果；实时状态；Guard 失败策略 | 代理失败、EXE 缺失、MSIX 超时都指出首个失败阶段；历史 PID 不显示为当前运行；双击不重复创建进程；Guard 明确报告关闭/重启结果 |
| 第二批：实例使用流程 | 内置模板、选择原版或空白实例、复制启动配置、创建结果摘要 | 同一安装可建两个独立目录实例；复制不带登录数据；快捷方式失败能原地补做；新增模板变量在 EXE/MSIX 均按同一策略生效 |
| 第三批：并发与维护 | 缩小全局锁范围、重新定位、入口修复 | 慢代理检查不阻塞无关编辑；配置中途变更会重新检查；两个启动共享同一内核时无竞态；安装更新和工具升级保留实例身份 |
| 需求出现后 | 设置继承、配置导入导出、用户脚本 | 显式区分继承/直连/绑定；导入不带机器路径、运行记录和凭据；脚本有单独的成功/失败/清理契约 |

需要覆盖的故障场景包括：代理始终不可用；preflight 后代理失效；目标已运行；包更新导致路径变化；辅助进程残留；回执超时后迟到；启动中修改代理绑定；源实例运行时请求复制数据。测试应使用受控进程和代理夹具，实机验收再确认 Codex/Claude 的实际行为。这里列的是后续验收设计，本次未执行。

实现第一批不必改变 schema，也不用立即拆成多张应用表。第二批可先增加模板解析层，将旧 `App` 映射为内部实例对象，保持现有 ID、目录与快捷方式。需要显式继承或共享安装来源时，再做 schema 迁移并保存备份；运行中的旧代理快照继续有效，避免改绑定后 Guard 错误替换现有会话。

**7. 不宜直接迁入的能力与研究边界**

PAL 的 NSIS 构建机制及“每个应用生成包装器”的分发方式，与当前 Node CLI 统一入口不同。借鉴其配置结构即可；不需要换语言或每个实例编译一个 EXE。Prism 的 Java、游戏资源和账号流程同样不应引入。

Prism 的通用命令钩子很灵活，但 App Proxy 当前主要需求是可靠代理启动。先提供内置步骤，等到存在具体用户需求再开放任意脚本。尤其不要把关停外部代理、恢复应用数据等必要清理交给可能不执行的后置命令。[Prism 命令实现](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/launch/steps/LaunchCommand.cpp)。

Prism 设置界面有代理选项，但所读实现将它设置为 Qt 应用级网络代理，不能据此认为它能替代 App Proxy 对外部桌面应用的代理注入或 Guard。此次阅读也没有找到可以直接替代当前 MSIX 包上下文启动和存储处理的实现。[Prism 代理配置入口](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/Application.cpp#L1817)。

文档和实现有版本差异：Prism 的 Custom Commands 网页描述命令工作目录为 launcher 工作目录，而本次提交在创建 pre-load/pre-launch/post-exit 步骤时明确设置 `gameRoot()`。本文涉及执行顺序与路径时以所读提交为依据。[网页说明](https://prismlauncher.org/wiki/help-pages/custom-commands/)，[源码设置](https://github.com/PrismLauncher/PrismLauncher/blob/d06c8a742894229d4967fb0bf16b9bf40cd377d3/launcher/minecraft/MinecraftInstance.cpp#L1174)。

从工程投入看，最有收益的组合是：PortableApps 的适配配置 + Prism 的实例体验和启动任务 + 当前项目已有的 sing-box、进程身份校验和 MSIX 适配。先完成第一、二批，就能让工具从“登记 EXE 的终端脚本”演进为清晰的实例 launcher；图形界面可以在流程稳定后复用同一套服务接口。
