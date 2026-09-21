# 桌面快捷方式创建修复

## 原因

正式 Claude 实例的创建请求停在已接收、尚未生成临时链接的阶段。计划中的启动器为 `\\?\D:\app_proxy\rust\target\release\app-proxy-host.exe`，来自 Windows 返回的实际进程路径。

新增原生回归以 `canonicalize` 产生相同形式的路径，修复前在 Shell Link 编码阶段稳定返回 `ShortcutCom / 0x80070057`（参数无效）。旧界面把这个系统错误折叠成 `COORDINATOR_OPERATION_FAILED`，又重复输出请求编号，不能解释结果。

## 修改

- 在 Shell Link 写入和核验字段时，将合法的扩展驱动器路径转换为普通驱动器路径。原有持久记录保持不变，未完成创建可直接恢复；文件身份与内容校验仍然保留。
- 保留快捷方式 COM 错误的数字错误码经过后台通信传回前台，并将常见错误解释为中文。
- 普通界面按结果、位置、原因展示，不再打印请求 UUID、数据目录或恢复命令。JSON 查询仍保留请求编号。
- 创建确认只显示创建与返回；未完成操作提供“继续处理（重试创建/移除/恢复）”。查询历史记录与核验当前链接继续区分。

## 验证

- 原生快捷方式测试：33 通过，1 个需外部 EXE 的图标 fixture 未执行。
- CLI 合约：3 通过；覆盖已有文件被修改时保留文件、失败提示包含原因和后续操作、普通输出不显示请求 UUID。
- 显式真实桌面测试：创建、核验、移除隔离 fixture 链接通过，未启动其目标程序。
- Clippy（workspace/all-targets，警告视为错误）通过；Release 前台构建通过；fmt/diff check 通过。

正式后台更新最初的管理员授权返回“操作已被用户取消”，当时恢复了原版前台与协调进程。用户再次授权后，完成 Release 后台构建与正式前后台替换，并重新安装监听。旧协调进程持有监听登记文件，维护时先停止旧协调进程再完成登记文件备份。

正式原请求恢复成功，生成 `C:\Users\Admin\Desktop\Claude 原版 - cb7abe97.lnk`，配置 revision 从 7 更新至 8。原生 `shortcut check` 核验通过，Shell 读取的目标为原 Release 目录中的 `app-proxy-host.exe`，参数为原实例启动入口，图标来自已保存的 Claude 图标。此次没有点击链接重新启动 Claude。

新版协调进程 PID 44264、监听 PID 30712；监听 generation 为 `e6d23e79-85bc-47c9-aaf5-29ab275892df`，受保护副本与 Release 后台 SHA-256 相同。最终查询为 `phase=active`、`listener=active_etw`、登录入口 `ready=true`，既有 Claude 代理会话保持。安装命令曾因同时创建快捷方式导致配置 revision 变化而停止其后续登录登记步骤；已有登录入口未改动，最终独立查询确认正常。证据在 `.tools/shortcut-fix/installed-link.json`、`deployment.json`、`guard-status.json`。

## 后续原版入口命名调整

用户要求原版不显示“原版”和短 ID。源码已改为原版使用应用名（例如 `Claude.lnk`），独立数据实例保留原命名。现有服务层 4 项回归、Clippy 及 Release 库编译通过。第一次后台更新授权取消后，先通过普通用户权限维护了现有入口（见下文）。

当前入口已通过普通用户权限完成更新：短暂停止协调进程取得 Store 独占锁，使用原有已核验的 Plan，通过 Store 的受理/发布/回执流程移除旧入口并创建 `Claude.lnk`，保留原实例、启动器及图标。随后由原登录任务恢复协调进程。再次核验新链接通过、旧链接不存在、ETW active、登录启动 ready。临时维护工具源代码保存在 `.tools/shortcut-fix/shortcut_name_maintenance_20260921.rs`，结果在 `name-result.json`；未点击链接启动 Claude。

用户再次要求更新后，旧监听退役和前后台 Release 二进制更新成功，新协调进程 PID 24252（11:33:31 启动）。安装新版监听的第二次 UAC 一度取消，期间状态为 `needs_authorization`，登录入口核验 `ready=false`。随后用户要求重新授权，安装成功：监听 generation 为 `c7646e3d-eb26-4f4b-b892-c3181c91947c`，安装副本与 Release 后台 SHA-256 一致。最终独立查询为保护 `active`、监听 `active_etw`、登录启动 `ready=true`、无诊断错误；桌面 `Claude.lnk` 通过核验。命名规则现已部署完成。本次未重新进行应用关闭测速。最终证据 `.tools/shortcut-fix/name-deploy-status.json`、`name-deployment.json`。
