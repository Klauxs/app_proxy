# Claude 原版漏纠正与实际代理连接验证：2026-09-21

## 现场与原因

用户从任务栏或开始菜单打开 Claude。排查时主进程 PID 23184（00:44:01 创建）未带 `--proxy-server`，代理内核仍在监听。首次进程检查没有发现协调进程；随后状态查询启动的协调进程创建于 00:45:08，ETW 恢复后状态一直为 `INSTANCE_PROCESS_UNKNOWN`。这只能证明排查时后台曾不在运行，不能确定它此前退出的时刻或原因，也不能据此认定 ETW 在正常运行时丢失了该启动事件。

原生命令行/映像身份探针确认主进程为 Main + Target + Mismatched；同映像的已知辅助进程均为 Auxiliary + Unknown，因为 Claude 给辅助进程显式传入默认 `--user-data-dir`，而原版主进程没有这个参数。兜底扫描将辅助进程的目录归属未知当成了整轮阻塞，导致已确认未使用代理的主进程无法纠正。

## 修复

- 已识别的辅助进程不再否决独立确认的主进程；它们仍不作为停止目标，仅剩辅助进程时仍报告占用，不能判为应用已退出。
- 未知进程角色、无法确认的主进程和重启前占用检查保持阻塞；精确进程身份检查保持不变。
- 现有可选异步计时补充事件检查/登记失败阶段及错误码，便于区分事件路径与兜底路径。

新增回归覆盖原版主进程与带显式默认数据目录的辅助进程并存、只剩辅助进程、未知角色阻塞。Guard 定向回归 46 项通过，workspace/all-targets Clippy `-D warnings`、fmt、Release 构建通过；本轮没有重跑此前的 470 项完整回归。

## 正式部署与实测

新版已覆盖原有 `rust/target/release`，经 Windows 管理员授权重新登记受保护监听组件，安装副本与 Release SHA-256 一致。没有更改代理节点、原版数据或实例配置。

1. **补救已运行应用**：Guard 自动关闭现场未带代理的 PID 23184，重启为 PID 22788；确认代理参数及网络子进程 PID 50980 到 `127.0.0.1:63658` 的实际 TCP 连接，另一端为 sing-box PID 49776。
2. **正常应用入口复测**：关闭上述确切纠正进程后，通过 `shell:AppsFolder\Claude_pzs8sxrjxfjjc!Claude` 激活正式应用。新建未带代理主进程 PID 54040 到确认退出 **453.175 ms**；到带代理新主进程 PID 636 创建 **2597.157 ms**。网络子进程 PID 25988 与代理端口建立 10 条连接，逐条匹配 sing-box 接收端。此样本区分了关闭耗时与整个重启耗时。

代理连接证据说明本次 Claude 网络服务实际使用了代理，不代表已测试发送消息等全部业务流量，也不保证所有类型的连接均走代理。

本地证据保存在 `.tools/guard-original-recovery/`：`recovered-attempt.json`、`network-before-retest.json`、`entry-attempt.json`、`entry-result.json`、`entry-connections.json`、`timing.ndjson` 及测试构建日志。临时计时结束后，通过已核验的共享守护登录任务启动正式后台；保留纠正后的 Claude 运行供用户使用。

最后核验：前台命令退出后，同一协调进程持续运行超过 92 秒，登录任务为 Running，ETW active、保护 active、登录登记 ready；仍可见 11 条 Claude 网络服务到代理端口的已建立连接，计时文件未继续增长。证据为 `final-status.json` 和 `resident-result.json`。本轮手动触发并检查了已登记的登录任务，没有注销 Windows 验证真实登录触发，也没有确定先前后台退出的原因。
