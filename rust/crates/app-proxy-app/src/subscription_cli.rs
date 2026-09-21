//! Subscription commands share the existing durable configuration/core protocols.
use crate::{
    coordinator, core_cli,
    exit::{self, Failure, fail},
    foreground::Foreground,
    proxy_cli::Command,
    subscription_preview::{NodeSummary, PreviewRequest, PreviewStatus, SavedPage, StageRequest},
};
use app_proxy_core::{
    core_control::CoreAction,
    model::{Endpoint, NetworkBinding},
    registry::{ConfigAction, ConfigRequest, SubscriptionChanges, SubscriptionEdit},
};
use app_proxy_windows::{
    Error,
    config_transaction::{ConfigOutcome, ConfigRequestStatus},
    subscription_stage::StagedSubscription,
};
use std::{
    io::{self, IsTerminal, Write},
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

mod selection;

fn show_groups(nodes: &[&NodeSummary], selected: &[usize]) -> Vec<(&'static str, Vec<usize>)> {
    let groups = selection::groups(nodes.iter().map(|n| n.name.as_str()));
    println!("地区按节点名称识别；* 表示已选。多选后自动测速切换。");
    for (group, (region, indices)) in groups.iter().enumerate() {
        println!("\n[G{}] {}（{} 个节点）", group + 1, region, indices.len());
        for index in indices {
            show_node(*index, nodes[*index], selected.contains(index));
        }
    }
    groups
}

fn print(value: &impl serde::Serialize) -> Result<(), Failure> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|_| fail(exit::INTERNAL, "OUTPUT_ENCODING_FAILED"))?
    );
    Ok(())
}
fn display(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn show_node(index: usize, node: &NodeSummary, selected: bool) {
    println!(
        "{}. {}{}  {:?} {}:{}",
        index + 1,
        if selected { "* " } else { "" },
        display(&node.name),
        node.protocol,
        display(&node.server),
        node.port
    );
}

pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    run_with_foreground(root, command, json, &mut Foreground::new())
        .await
        .map(|_| ())
}

pub(crate) async fn run_with_foreground(
    root: PathBuf,
    command: Command,
    json: bool,
    foreground: &mut Foreground,
) -> Result<Option<Uuid>, Failure> {
    foreground.check()?;
    match command {
        Command::Nodes { id } => {
            let page = saved_nodes(&root, id).await?;
            if json {
                print(&page)?;
            } else {
                let groups = show_groups(
                    &page.nodes.iter().map(|n| &n.node).collect::<Vec<_>>(),
                    &page
                        .nodes
                        .iter()
                        .enumerate()
                        .filter_map(|(i, n)| page.selected_node_ids.contains(&n.id).then_some(i))
                        .collect::<Vec<_>>(),
                );
                for (_, indices) in groups {
                    for index in indices {
                        println!("{}. {}", index + 1, page.nodes[index].id);
                    }
                }
            }
            Ok(None)
        }
        Command::Select {
            id,
            node,
            apply_to_running,
        } => {
            let page = saved_nodes(&root, id).await?;
            let selected = if !node.is_empty() {
                if node
                    .iter()
                    .any(|id| !page.nodes.iter().any(|n| n.id == *id))
                {
                    return Err(fail(exit::INVALID, "SELECTED_NODE_NOT_FOUND"));
                }
                node
            } else {
                if json || !core_cli::interactive() {
                    return Err(fail(exit::INVALID, "请提供节点 ID，或在终端交互选择。"));
                }
                let groups = show_groups(
                    &page.nodes.iter().map(|n| &n.node).collect::<Vec<_>>(),
                    &page
                        .nodes
                        .iter()
                        .enumerate()
                        .filter_map(|(i, n)| page.selected_node_ids.contains(&n.id).then_some(i))
                        .collect::<Vec<_>>(),
                );
                choose_nodes(page.nodes.len(), &groups, foreground)
                    .await?
                    .into_iter()
                    .map(|i| page.nodes[i].id)
                    .collect()
            };
            commit(
                &root,
                StagedSubscription {
                    request: ConfigRequest {
                        request_id: Uuid::new_v4(),
                        expected_revision: page.revision,
                        action: ConfigAction::EditSubscriptionProfile {
                            profile_id: id,
                            edit: SubscriptionEdit::Select {
                                expected_source_revision: page.source_revision,
                                node_ids: selected,
                            },
                        },
                    },
                    changes: SubscriptionChanges {
                        added: vec![],
                        removed: vec![],
                        unsupported: 0,
                    },
                },
                apply_to_running,
                json,
                foreground,
            )
            .await?;
            Ok(Some(id))
        }
        Command::Import {
            name,
            url_stdin,
            node,
            via,
            apply_to_running,
        } => {
            if node.is_empty() && (json || !core_cli::interactive()) {
                return Err(fail(
                    exit::INVALID,
                    "非交互导入请用 --node 指定准确节点名。",
                ));
            }
            let url = read_url(url_stdin, foreground).await?;
            let fallback_title = selection::source_label(&url);
            prepare_route(&root, via, apply_to_running, json, foreground).await?;
            let id = Uuid::new_v4();
            let profile_id = Uuid::new_v4();
            let result = async {
                let (nodes, title) = preview(
                    &root,
                    id,
                    PreviewRequest::Import {
                        url,
                        network: network(via),
                    },
                    foreground,
                )
                .await?;
                let selected_names = if !node.is_empty() {
                    if node
                        .iter()
                        .any(|name| !nodes.iter().any(|n| n.name == *name))
                    {
                        return Err(fail(exit::INVALID, "SELECTED_NODE_NOT_FOUND"));
                    }
                    node
                } else {
                    let groups = show_groups(&nodes.iter().collect::<Vec<_>>(), &[]);
                    choose_nodes(nodes.len(), &groups, foreground)
                        .await?
                        .into_iter()
                        .map(|i| nodes[i].name.clone())
                        .collect()
                };
                let catalog = coordinator::catalog(root.clone())
                    .await
                    .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
                let name = name.unwrap_or_else(|| {
                    selection::automatic_name(
                        title.as_deref().unwrap_or(&fallback_title),
                        &selected_names,
                        &app_proxy_windows::local_time::name_timestamp(),
                        catalog.profiles.iter().map(|p| p.name.as_str()),
                    )
                });
                let mut reservations = Vec::new();
                let mut endpoint = None;
                for _ in 0..64 {
                    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                        .map_err(|_| fail(exit::UNAVAILABLE, "LOCAL_PROXY_PORT_UNAVAILABLE"))?;
                    let port = listener
                        .local_addr()
                        .map_err(|_| fail(exit::UNAVAILABLE, "LOCAL_PROXY_PORT_UNAVAILABLE"))?
                        .port();
                    reservations.push(listener);
                    if !catalog.profiles.iter().any(|p| p.endpoint.port == port) {
                        endpoint = Some(Endpoint {
                            host: Ipv4Addr::LOCALHOST.into(),
                            port,
                        });
                        break;
                    }
                }
                let staged = stage(
                    &root,
                    id,
                    StageRequest::Import {
                        profile_id,
                        name,
                        endpoint: endpoint.ok_or_else(|| {
                            fail(exit::UNAVAILABLE, "LOCAL_PROXY_PORT_UNAVAILABLE")
                        })?,
                        selected_names,
                    },
                    foreground,
                )
                .await?;
                let result = commit(&root, staged, apply_to_running, json, foreground).await;
                drop(reservations);
                result
            }
            .await;
            let _ = coordinator::subscription_preview_close(root, id).await;
            result.map(|_| Some(profile_id))
        }
        Command::Refresh {
            id: profile_id,
            via,
            apply_to_running,
        } => {
            // Reject non-subscription targets before starting a download route.
            saved_nodes(&root, profile_id).await?;
            prepare_route(&root, via, apply_to_running, json, foreground).await?;
            let id = Uuid::new_v4();
            let result = async {
                preview(
                    &root,
                    id,
                    PreviewRequest::Refresh {
                        profile_id,
                        network: network(via),
                    },
                    foreground,
                )
                .await?;
                let staged = stage(&root, id, StageRequest::Refresh {}, foreground).await?;
                commit(&root, staged, apply_to_running, json, foreground).await
            }
            .await;
            let _ = coordinator::subscription_preview_close(root, id).await;
            result.map(|_| Some(profile_id))
        }
        _ => unreachable!(),
    }
}

async fn read_url(from_stdin: bool, foreground: &mut Foreground) -> Result<String, Failure> {
    if !from_stdin && !core_cli::interactive() {
        return Err(fail(exit::INVALID, "请通过 --url-stdin 提供订阅地址。"));
    }
    foreground.check()?;
    if io::stdin().is_terminal() {
        eprint!("请输入订阅地址（不回显）：");
        io::stderr()
            .flush()
            .map_err(|_| fail(exit::INTERNAL, "PROMPT_WRITE_FAILED"))?;
    }
    let url = foreground
        .read_secret_line(8192)
        .await?
        .ok_or_else(|| fail(exit::ACTION_REQUIRED, "已返回；未导入订阅。"))?;
    app_proxy_core::subscription::source_url(&url)
        .map_err(|_| fail(exit::INVALID, "SUBSCRIPTION_URL_INVALID"))?;
    Ok(url)
}
fn network(via: Option<Uuid>) -> NetworkBinding {
    via.map(|profile_id| NetworkBinding::Profile { profile_id })
        .unwrap_or(NetworkBinding::Direct {})
}
async fn prepare_route(
    root: &Path,
    via: Option<Uuid>,
    apply: bool,
    json: bool,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    if let Some(profile) = via {
        core_cli::ensure_download_profile(root.into(), profile, apply, json, foreground).await?;
    }
    foreground.check()
}
async fn saved_nodes(root: &Path, id: Uuid) -> Result<SavedPage, Failure> {
    let mut page = coordinator::subscription_nodes(root.into(), id, 0, None)
        .await
        .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    while let Some(offset) = page.next_offset {
        let next = coordinator::subscription_nodes(root.into(), id, offset, Some(page.revision))
            .await
            .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
        page.nodes.extend(next.nodes);
        page.next_offset = next.next_offset;
    }
    Ok(page)
}
async fn choose_nodes(
    count: usize,
    groups: &[(&str, Vec<usize>)],
    foreground: &mut Foreground,
) -> Result<Vec<usize>, Failure> {
    loop {
        eprint!("选择节点 [1-{count}]：如 1,3-5；G1 选整组；all 全选；0/回车返回：");
        io::stderr()
            .flush()
            .map_err(|_| fail(exit::INTERNAL, "PROMPT_WRITE_FAILED"))?;
        let Some(input) = foreground.read_optional_line().await? else {
            return Err(fail(exit::ACTION_REQUIRED, "已返回；原配置保留。"));
        };
        if input.trim().is_empty() || input.trim() == "0" {
            return Err(fail(exit::ACTION_REQUIRED, "已返回；原配置保留。"));
        }
        if let Some(selected) = selection::parse(&input, count, groups) {
            return Ok(selected);
        }
        eprintln!("请输入有效节点编号、范围或地区组编号，可用逗号分隔。");
    }
}
async fn preview(
    root: &Path,
    id: Uuid,
    request: PreviewRequest,
    foreground: &mut Foreground,
) -> Result<(Vec<NodeSummary>, Option<String>), Failure> {
    foreground.check()?;
    eprintln!("正在读取订阅…");
    let mut response = coordinator::subscription_preview(root.into(), id, request).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    loop {
        foreground.check()?;
        match response {
            Ok(page) => match page.status {
                PreviewStatus::Failed { code } => return Err(fail(exit::UNAVAILABLE, code)),
                PreviewStatus::Ready {
                    nodes: _,
                    unsupported,
                } => {
                    if unsupported > 0 {
                        eprintln!("已忽略 {unsupported} 个不支持的条目。");
                    }
                    let mut nodes = page.nodes;
                    let mut offset = page.next_offset;
                    while let Some(next) = offset {
                        foreground.check()?;
                        let page = coordinator::subscription_preview_page(root.into(), id, next)
                            .await
                            .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
                        nodes.extend(page.nodes);
                        offset = page.next_offset;
                    }
                    return Ok((nodes, page.title));
                }
                PreviewStatus::Pending {} => {}
            },
            Err(Error::Invalid(app_proxy_core::error_code::SUBSCRIPTION_PREVIEW_EXPIRED)) => {
                return Err(fail(exit::UNAVAILABLE, "订阅预览已失效，请重新读取。"));
            }
            Err(error) if retryable(&error) => {}
            Err(error) => return Err(rpc_failure(error)),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(fail(exit::UNAVAILABLE, "SUBSCRIPTION_PREVIEW_TIMEOUT"));
        }
        tokio::select! { biased;
            _ = foreground.cancelled() => return Err(fail(exit::ACTION_REQUIRED, "已取消订阅下载；原配置保留。")),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
        response = coordinator::subscription_preview_page(root.into(), id, 0).await;
    }
}
async fn stage(
    root: &Path,
    preview_id: Uuid,
    request: StageRequest,
    foreground: &mut Foreground,
) -> Result<StagedSubscription, Failure> {
    let id = Uuid::new_v4();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        foreground.check()?;
        match coordinator::subscription_stage(root.into(), preview_id, id, request.clone()).await {
            Ok(staged) => return Ok(staged),
            Err(Error::Invalid(app_proxy_core::error_code::SUBSCRIPTION_STAGE_PENDING)) => {}
            Err(error) if retryable(&error) => {}
            Err(error) => return Err(rpc_failure(error)),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(fail(
                exit::UNAVAILABLE,
                "订阅准备结果未确认；未提交配置，可重新读取订阅。",
            ));
        }
        tokio::select! { biased;
            _ = foreground.cancelled() => return Err(fail(exit::ACTION_REQUIRED, "已返回；未提交订阅配置。")),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    }
}

fn retryable(error: &Error) -> bool {
    matches!(
        error,
        Error::Io(_)
            | Error::Invalid(
                app_proxy_core::error_code::IPC_CONNECT_TIMEOUT
                    | "IPC_FRAME_TIMEOUT"
                    | "IPC_CONNECTION_CLOSED"
            )
    )
}
fn rpc_failure(error: Error) -> Failure {
    fail(
        exit::UNAVAILABLE,
        match error {
            Error::Invalid(code) => code,
            _ => "SUBSCRIPTION_RPC_FAILED",
        },
    )
}
async fn commit(
    root: &Path,
    staged: StagedSubscription,
    apply: bool,
    json: bool,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    foreground.check()?;
    let request = staged.request;
    let id = request.request_id;
    let summary = saved_summary(&request.action, &staged.changes);
    let prepare =
        if let ConfigAction::EditSubscriptionProfile { profile_id, edit } = &request.action {
            Some(CoreAction::PrepareSubscription {
                expected_revision: request.expected_revision,
                profile_id: *profile_id,
                edit: edit.clone(),
            })
        } else {
            None
        };
    if json {
        eprintln!("请求编号：{id}；结果不明时运行 proxy request {id} 查询。");
    }
    let result = match coordinator::configure(root.into(), request).await {
        Ok(outcome) => Some(outcome),
        Err(_) => match coordinator::request_status(root.into(), id).await {
            Ok(Some(ConfigRequestStatus::Complete { outcome })) => Some(outcome),
            _ => None,
        },
    };
    match result {
        Some(ConfigOutcome::Applied { receipt }) => {
            if json {
                print(
                    &serde_json::json!({"request_id":id,"receipt":receipt,"changes":staged.changes}),
                )?;
            } else {
                println!("{summary}");
            }
            Ok(())
        }
        Some(ConfigOutcome::Rejected { code, .. }) if code == "CORE_RECONFIGURATION_REQUIRED" => {
            foreground.check()?;
            core_cli::prepare_and_apply_with_foreground(
                root.into(),
                prepare.ok_or_else(|| fail(exit::ACTION_REQUIRED, code))?,
                apply,
                json,
                foreground,
            )
            .await
        }
        Some(outcome @ ConfigOutcome::Rejected { .. }) => {
            if json {
                print(&serde_json::json!({"request_id":id,"outcome":outcome}))?;
            }
            let ConfigOutcome::Rejected { code, .. } = outcome else {
                unreachable!()
            };
            Err(fail(exit::INVALID, code))
        }
        None => Err(fail(
            exit::UNCONFIRMED,
            format!("结果未确认，请用 proxy request {id} 查询；不要自动重新提交。"),
        )),
    }
}

fn saved_summary(action: &ConfigAction, changes: &SubscriptionChanges) -> String {
    let mode = |count| {
        if count > 1 {
            "自动测速切换"
        } else {
            "固定节点"
        }
    };
    match action {
        ConfigAction::AddProfile { profile } => {
            let count = profile.selected_node_ids().len();
            format!(
                "\n已保存：{}\n{count} 个节点 · {}",
                display(&profile.name),
                mode(count)
            )
        }
        ConfigAction::EditSubscriptionProfile {
            edit: SubscriptionEdit::Select { node_ids, .. },
            ..
        } => {
            format!(
                "\n已保存：{} 个节点 · {}",
                node_ids.len(),
                mode(node_ids.len())
            )
        }
        _ => format!(
            "\n订阅已刷新：新增 {} 个，移除 {} 个。",
            changes.added.len(),
            changes.removed.len()
        ),
    }
}
