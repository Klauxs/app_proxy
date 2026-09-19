//! Subscription commands share the existing durable configuration/core protocols.
use crate::{
    coordinator, core_cli,
    foreground::Foreground,
    instance_cli::{Failure, fail},
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
    io::{self, BufRead, IsTerminal, Read, Write},
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

fn print(value: &impl serde::Serialize) -> Result<(), Failure> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
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
    let mut foreground = Foreground::new();
    match command {
        Command::Nodes { id } => {
            let page = saved_nodes(&root, id).await?;
            if json {
                print(&page)
            } else {
                for (index, node) in page.nodes.iter().enumerate() {
                    show_node(index, &node.node, node.id == page.selected_node_id);
                    println!("   {}", node.id);
                }
                Ok(())
            }
        }
        Command::Select {
            id,
            node,
            apply_to_running,
        } => {
            let page = saved_nodes(&root, id).await?;
            let selected = if let Some(id) = node {
                page.nodes
                    .iter()
                    .find(|n| n.id == id)
                    .ok_or_else(|| fail(2, "SELECTED_NODE_NOT_FOUND"))?
                    .id
            } else {
                if json || !core_cli::interactive() {
                    return Err(fail(2, "请提供节点 ID，或在终端交互选择。"));
                }
                for (index, node) in page.nodes.iter().enumerate() {
                    show_node(index, &node.node, node.id == page.selected_node_id);
                }
                page.nodes[choose_node(page.nodes.len(), &mut foreground).await?].id
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
                                node_id: selected,
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
                &mut foreground,
            )
            .await
        }
        Command::Import {
            name,
            url_stdin,
            node,
            via,
            apply_to_running,
        } => {
            if node.is_none() && (json || !core_cli::interactive()) {
                return Err(fail(2, "非交互导入请用 --node 指定准确节点名。"));
            }
            let url = read_url(url_stdin, &mut foreground).await?;
            prepare_route(&root, via, apply_to_running, json, &mut foreground).await?;
            let id = Uuid::new_v4();
            let result = async {
                let nodes = preview(
                    &root,
                    id,
                    PreviewRequest::Import {
                        url,
                        network: network(via),
                    },
                    &mut foreground,
                )
                .await?;
                let selected_name = if let Some(name) = node {
                    if !nodes.iter().any(|n| n.name == name) {
                        return Err(fail(2, "SELECTED_NODE_NOT_FOUND"));
                    }
                    name
                } else {
                    for (index, node) in nodes.iter().enumerate() {
                        show_node(index, node, false);
                    }
                    nodes[choose_node(nodes.len(), &mut foreground).await?]
                        .name
                        .clone()
                };
                let catalog = coordinator::catalog(root.clone())
                    .await
                    .map_err(|e| fail(3, e.to_string()))?;
                let mut reservations = Vec::new();
                let mut endpoint = None;
                for _ in 0..64 {
                    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                        .map_err(|_| fail(3, "LOCAL_PROXY_PORT_UNAVAILABLE"))?;
                    let port = listener
                        .local_addr()
                        .map_err(|_| fail(3, "LOCAL_PROXY_PORT_UNAVAILABLE"))?
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
                        profile_id: Uuid::new_v4(),
                        name,
                        endpoint: endpoint
                            .ok_or_else(|| fail(3, "LOCAL_PROXY_PORT_UNAVAILABLE"))?,
                        selected_name,
                    },
                    &mut foreground,
                )
                .await?;
                let result = commit(&root, staged, apply_to_running, json, &mut foreground).await;
                drop(reservations);
                result
            }
            .await;
            let _ = coordinator::subscription_preview_close(root, id).await;
            result
        }
        Command::Refresh {
            id: profile_id,
            via,
            apply_to_running,
        } => {
            // Reject non-subscription targets before starting a download route.
            saved_nodes(&root, profile_id).await?;
            prepare_route(&root, via, apply_to_running, json, &mut foreground).await?;
            let id = Uuid::new_v4();
            let result = async {
                preview(
                    &root,
                    id,
                    PreviewRequest::Refresh {
                        profile_id,
                        network: network(via),
                    },
                    &mut foreground,
                )
                .await?;
                let staged = stage(&root, id, StageRequest::Refresh {}, &mut foreground).await?;
                commit(&root, staged, apply_to_running, json, &mut foreground).await
            }
            .await;
            let _ = coordinator::subscription_preview_close(root, id).await;
            result
        }
        _ => unreachable!(),
    }
}

async fn read_url(from_stdin: bool, foreground: &mut Foreground) -> Result<String, Failure> {
    if !from_stdin && !core_cli::interactive() {
        return Err(fail(2, "请通过 --url-stdin 提供订阅地址。"));
    }
    foreground.check()?;
    let hidden = if io::stdin().is_terminal() {
        eprint!("请输入订阅地址（不回显）：");
        io::stderr()
            .flush()
            .map_err(|_| fail(10, "PROMPT_WRITE_FAILED"))?;
        Some(
            app_proxy_windows::console::HiddenInput::begin()
                .map_err(|_| fail(3, "SECRET_INPUT_UNAVAILABLE"))?,
        )
    } else {
        None
    };
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = io::stdin()
            .lock()
            .take(8195)
            .read_until(b'\n', &mut bytes)
            .map(|_| bytes);
        let _ = sender.send(result);
    });
    let result = tokio::select! {
        biased;
        _ = foreground.cancelled() => Err(fail(5, "已返回；未导入订阅。")),
        result = receiver => result.map_err(|_| fail(2, "SUBSCRIPTION_URL_INPUT_FAILED"))?
            .map_err(|_| fail(2, "SUBSCRIPTION_URL_INPUT_FAILED")),
    };
    if hidden.is_some() {
        eprintln!();
    }
    drop(hidden);
    let mut url = String::from_utf8(result?).map_err(|_| fail(2, "SUBSCRIPTION_URL_INVALID"))?;
    if url.ends_with('\n') {
        url.pop();
        if url.ends_with('\r') {
            url.pop();
        }
    }
    app_proxy_core::subscription::source_url(&url)
        .map_err(|_| fail(2, "SUBSCRIPTION_URL_INVALID"))?;
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
        .map_err(|e| fail(3, e.to_string()))?;
    while let Some(offset) = page.next_offset {
        let next = coordinator::subscription_nodes(root.into(), id, offset, Some(page.revision))
            .await
            .map_err(|e| fail(3, e.to_string()))?;
        page.nodes.extend(next.nodes);
        page.next_offset = next.next_offset;
    }
    Ok(page)
}
async fn choose_node(count: usize, foreground: &mut Foreground) -> Result<usize, Failure> {
    loop {
        eprint!("选择节点 [1-{count}]，回车返回：");
        io::stderr()
            .flush()
            .map_err(|_| fail(10, "PROMPT_WRITE_FAILED"))?;
        let Some(input) = foreground.read_optional_line().await? else {
            return Err(fail(5, "已返回；原配置保留。"));
        };
        if input.trim().is_empty() {
            return Err(fail(5, "已返回；原配置保留。"));
        }
        if let Ok(n) = input.trim().parse::<usize>()
            && n > 0
            && n <= count
        {
            return Ok(n - 1);
        }
        eprintln!("请输入列表中的编号。");
    }
}
async fn preview(
    root: &Path,
    id: Uuid,
    request: PreviewRequest,
    foreground: &mut Foreground,
) -> Result<Vec<NodeSummary>, Failure> {
    foreground.check()?;
    eprintln!("正在读取订阅…");
    let mut response = coordinator::subscription_preview(root.into(), id, request).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    loop {
        foreground.check()?;
        match response {
            Ok(page) => match page.status {
                PreviewStatus::Failed { code } => return Err(fail(3, code)),
                PreviewStatus::Ready {
                    nodes: _,
                    unsupported,
                } => {
                    eprintln!("订阅读取完成；不支持的条目 {unsupported} 个。");
                    let mut nodes = page.nodes;
                    let mut offset = page.next_offset;
                    while let Some(next) = offset {
                        foreground.check()?;
                        let page = coordinator::subscription_preview_page(root.into(), id, next)
                            .await
                            .map_err(|e| fail(3, e.to_string()))?;
                        nodes.extend(page.nodes);
                        offset = page.next_offset;
                    }
                    return Ok(nodes);
                }
                PreviewStatus::Pending {} => {}
            },
            Err(Error::Invalid("SUBSCRIPTION_PREVIEW_EXPIRED")) => {
                return Err(fail(3, "订阅预览已失效，请重新读取。"));
            }
            Err(error) if retryable(&error) => {}
            Err(error) => return Err(rpc_failure(error)),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(fail(3, "SUBSCRIPTION_PREVIEW_TIMEOUT"));
        }
        tokio::select! { biased;
            _ = foreground.cancelled() => return Err(fail(5, "已取消订阅下载；原配置保留。")),
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
            Err(Error::Invalid("SUBSCRIPTION_STAGE_PENDING")) => {}
            Err(error) if retryable(&error) => {}
            Err(error) => return Err(rpc_failure(error)),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(fail(3, "订阅准备结果未确认；未提交配置，可重新读取订阅。"));
        }
        tokio::select! { biased;
            _ = foreground.cancelled() => return Err(fail(5, "已返回；未提交订阅配置。")),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    }
}

fn retryable(error: &Error) -> bool {
    matches!(
        error,
        Error::Io(_)
            | Error::Invalid("IPC_CONNECT_TIMEOUT" | "IPC_FRAME_TIMEOUT" | "IPC_CONNECTION_CLOSED")
    )
}
fn rpc_failure(error: Error) -> Failure {
    fail(
        3,
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
    eprintln!(
        "新增节点：{}；删除节点：{}；不支持条目：{}。",
        staged
            .changes
            .added
            .iter()
            .map(|n| display(n))
            .collect::<Vec<_>>()
            .join("、"),
        staged
            .changes
            .removed
            .iter()
            .map(|n| display(n))
            .collect::<Vec<_>>()
            .join("、"),
        staged.changes.unsupported
    );
    eprintln!("请求编号：{id}；结果不明时运行 proxy request {id} 查询。");
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
                println!(
                    "代理配置已保存：{}，版本 {}。",
                    receipt.entity_id, receipt.revision
                );
            }
            Ok(())
        }
        Some(ConfigOutcome::Rejected { code, .. }) if code == "CORE_RECONFIGURATION_REQUIRED" => {
            foreground.check()?;
            core_cli::prepare_and_apply_with_foreground(
                root.into(),
                prepare.ok_or_else(|| fail(5, code))?,
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
            Err(fail(2, code))
        }
        None => Err(fail(
            6,
            format!("结果未确认，请用 proxy request {id} 查询；不要自动重新提交。"),
        )),
    }
}
