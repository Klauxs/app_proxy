//! Display runtime evidence separately from configuration and protection.
use crate::{
    coordinator,
    exit::{self, Failure, fail},
    launch_engine::{InstanceObservation, RuntimeStatus},
};
use app_proxy_core::launch::LaunchNetwork;
use std::path::Path;
use uuid::Uuid;

pub(crate) fn label(status: &RuntimeStatus, revision: u64) -> &'static str {
    if status.revision != revision {
        return "未确认（配置已变化）";
    }
    match &status.observation {
        InstanceObservation::Session {
            configuration_changed: Some(true),
            ..
        } => "运行中（下次启动配置已改变）",
        InstanceObservation::Session { .. } => "运行中（已核验会话）",
        InstanceObservation::Observed { .. } => "已观察到主进程",
        InstanceObservation::Absent {} => "当前会话未发现主进程",
        InstanceObservation::Pending { .. } => "启动请求待确认",
        InstanceObservation::Unknown { .. } => "未确认",
    }
}

pub async fn inspect(root: &Path, id: Uuid, json: bool) -> Result<(), Failure> {
    let catalog = coordinator::catalog(root.into())
        .await
        .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    let instance = catalog
        .instances
        .iter()
        .find(|i| i.id == id)
        .ok_or_else(|| fail(exit::INVALID, "INSTANCE_NOT_FOUND"))?;
    let application = catalog
        .applications
        .iter()
        .find(|a| a.id == instance.application_id)
        .ok_or_else(|| fail(exit::INVALID, "APPLICATION_NOT_FOUND"))?;
    let runtime = coordinator::runtime_status(root.into(), id)
        .await
        .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    let guard = coordinator::guard_status(root.into(), id).await.ok();
    if runtime.revision != catalog.revision
        || guard
            .as_ref()
            .is_some_and(|g| g.revision != catalog.revision)
        || coordinator::catalog(root.into())
            .await
            .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?
            .revision
            != catalog.revision
    {
        return Err(fail(
            exit::CONFLICT,
            "配置在查询期间发生变化，请重新查看实例详情。",
        ));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "instance": instance, "application": application, "runtime": runtime,
                "protection": guard,
                "protection_diagnostic": if guard.is_none() { Some("GUARD_STATUS_UNCONFIRMED") } else { None },
                "target_traffic_evidence": "not_observed"
            }))
            .map_err(|_| fail(exit::INTERNAL, "OUTPUT_ENCODING_FAILED"))?
        );
    } else {
        let display = |text: &str| {
            text.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        };
        println!(
            "{} · {} · {}\n实例编号 {id}",
            display(&instance.name),
            display(&application.name),
            if instance.isolated {
                "分身/独立数据"
            } else {
                "原版"
            }
        );
        println!("进程：{}", label(&runtime, catalog.revision));
        match instance.network {
            app_proxy_core::model::NetworkBinding::Direct {} => println!("下次启动：直连。"),
            app_proxy_core::model::NetworkBinding::Profile { profile_id } => {
                let name = catalog
                    .profiles
                    .iter()
                    .find(|p| p.id == profile_id)
                    .map(|p| display(&p.name))
                    .unwrap_or_else(|| profile_id.to_string());
                println!("下次启动：代理 {name}。");
            }
        }
        match &runtime.observation {
            InstanceObservation::Session {
                process,
                network,
                configuration_changed,
            } => {
                println!("已核验会话 PID {}", process.pid);
                match network {
                    LaunchNetwork::Direct {} => println!("本次会话启动时使用直连。"),
                    LaunchNetwork::Profile {
                        profile_id,
                        endpoint,
                        ..
                    } => println!(
                        "本次会话启动时使用代理 {profile_id}，入口 {}:{}。",
                        endpoint.host, endpoint.port
                    ),
                }
                if configuration_changed.is_none() {
                    println!("本次会话与当前配置的关系暂未确认。");
                }
            }
            InstanceObservation::Observed { process } => {
                println!("发现主进程 PID {}，尚无本工具的启动会话证据。", process.pid)
            }
            InstanceObservation::Pending { attempt_id } => {
                println!("启动请求 {attempt_id} 待确认。")
            }
            InstanceObservation::Unknown { code } => println!("进程检查诊断：{code}"),
            InstanceObservation::Absent {} => {}
        }
        if let Some(guard) = guard {
            println!(
                "保护：{} · 监听：{}",
                guard_label(&guard),
                crate::guard_cli::component(guard.listener)
            );
            if let Some(note) = guard.diagnostic {
                println!("保护诊断：{note}");
            }
        } else {
            println!("保护状态未确认，请稍后重试。");
        }
        println!("此查询只验证进程状态，未验证应用实际流量。");
    }
    Ok(())
}

pub(crate) fn guard_label(status: &crate::guard_control::GuardStatus) -> &'static str {
    use crate::guard_control::GuardPhase;
    match status.phase {
        GuardPhase::Disabled => "已关闭",
        GuardPhase::NeedsAuthorization => "待授权",
        GuardPhase::Starting => "检查中",
        GuardPhase::Active => "运行中",
        GuardPhase::Degraded => "降级",
        GuardPhase::Blocked => "受阻",
    }
}
