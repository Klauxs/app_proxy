//! Manual proxy configuration. Secrets are accepted over redirected stdin only,
//! never through process arguments, output or persisted request bodies.
use crate::{
    coordinator,
    instance_cli::{self, Failure, fail},
};
use app_proxy_core::{
    model::{Endpoint, ManualProtocol},
    registry::*,
};
use clap::{Args, Subcommand, ValueEnum};
use std::{
    io::{self, IsTerminal, Read},
    net::{Ipv4Addr, TcpListener},
    path::PathBuf,
};
use uuid::Uuid;

#[derive(Clone, ValueEnum)]
pub enum Protocol {
    Http,
    Socks5,
}

#[derive(Args)]
pub struct Node {
    #[arg(long, value_enum)]
    protocol: Protocol,
    #[arg(long)]
    host: String,
    #[arg(long)]
    port: u16,
    #[arg(long, requires = "password_stdin", conflicts_with = "no_auth")]
    username: Option<String>,
    /// 从重定向的标准输入读取密码；不将密码放入命令参数
    #[arg(long, requires = "username")]
    password_stdin: bool,
    /// 明确设置为无认证（更新时必须明确选择认证方式）
    #[arg(long)]
    no_auth: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// 导入订阅；地址通过交互输入或 --url-stdin 读取
    Import {
        #[arg(long)]
        name: Option<String>,
        /// 从标准输入读取一行订阅地址，避免命令行泄露 token
        #[arg(long)]
        url_stdin: bool,
        /// 选择准确节点名；可重复 --node 多选，省略时按地区交互选择
        #[arg(long)]
        node: Vec<String>,
        /// 使用本工具管理的代理下载；默认直接下载
        #[arg(long)]
        via: Option<Uuid>,
        #[arg(long)]
        apply_to_running: bool,
    },
    /// 刷新订阅，保留当前选中节点
    Refresh {
        id: Uuid,
        #[arg(long)]
        via: Option<Uuid>,
        #[arg(long)]
        apply_to_running: bool,
    },
    /// 列出订阅的已保存节点，不重新下载
    Nodes { id: Uuid },
    /// 选择已保存的订阅节点；省略节点 ID 时交互选择
    Select {
        id: Uuid,
        node: Vec<Uuid>,
        #[arg(long)]
        apply_to_running: bool,
    },
    /// 列出代理地址、入口和认证状态；不显示凭据
    List,
    /// 查看一个代理配置；不检查当前连通性
    Show { id: Uuid },
    /// 创建 HTTP/SOCKS5 代理；自动分配本地入口
    Create {
        #[arg(long)]
        name: String,
        #[command(flatten)]
        node: Node,
    },
    /// 替换上游和认证；保持 ID、本地入口和实例绑定
    Update {
        id: Uuid,
        /// 同意重启当前共享内核影响到的代理；不改变应用进程
        #[arg(long)]
        apply_to_running: bool,
        #[command(flatten)]
        node: Node,
    },
    /// 改名，不重启代理进程
    Rename { id: Uuid, name: String },
    /// 移除未被引用的配置；不删除凭据文件
    Remove {
        id: Uuid,
        #[arg(long)]
        apply_to_running: bool,
    },
    /// 查询原配置请求结果
    Request { id: Uuid },
}

impl Node {
    fn input(self, updating: bool) -> Result<ManualProxyInput, Failure> {
        if updating && self.username.is_none() && !self.no_auth {
            return Err(fail(
                2,
                "更新时请选择 --no-auth，或 --username 与 --password-stdin。",
            ));
        }
        let credentials = if let Some(username) = self.username {
            if io::stdin().is_terminal() {
                return Err(fail(2, "请通过重定向的标准输入提供密码，避免终端回显。"));
            }
            let mut bytes = Vec::new();
            io::stdin()
                .lock()
                .take(32771)
                .read_to_end(&mut bytes)
                .map_err(|_| fail(2, "PASSWORD_INPUT_FAILED"))?;
            let mut password =
                String::from_utf8(bytes).map_err(|_| fail(2, "INVALID_PROXY_PASSWORD"))?;
            if password.ends_with('\n') {
                password.pop();
                if password.ends_with('\r') {
                    password.pop();
                }
            }
            if password.len() > 32768 || password.contains('\0') {
                return Err(fail(2, "INVALID_PROXY_PASSWORD"));
            }
            Some(ProxyCredentialInput { username, password })
        } else {
            None
        };
        Ok(ManualProxyInput {
            protocol: match self.protocol {
                Protocol::Http => ManualProtocol::Http,
                Protocol::Socks5 => ManualProtocol::Socks5,
            },
            host: self.host,
            port: self.port,
            credentials,
        })
    }
}

fn print(value: &impl serde::Serialize) -> Result<(), Failure> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
    );
    Ok(())
}
fn display(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    if matches!(
        command,
        Command::Import { .. }
            | Command::Refresh { .. }
            | Command::Nodes { .. }
            | Command::Select { .. }
    ) {
        return crate::subscription_cli::run(root, command, json).await;
    }
    if let Command::Request { id } = command {
        return instance_cli::run(root, instance_cli::Command::Request { id }, json).await;
    }
    let catalog = coordinator::catalog(root.clone())
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    let action = match command {
        Command::List | Command::Show { .. } => {
            let selected = match command {
                Command::Show { id } => Some(id),
                _ => None,
            };
            let profiles: Vec<_> = catalog
                .profiles
                .iter()
                .filter(|p| selected.is_none_or(|id| p.id == id))
                .collect();
            if selected.is_some() && profiles.is_empty() {
                return Err(fail(2, "PROFILE_NOT_FOUND"));
            }
            if json {
                print(&serde_json::json!({"revision":catalog.revision,"profiles":profiles}))?;
            } else {
                for p in profiles {
                    println!(
                        "{}  {}  {}  入口 {}:{}  {}",
                        p.id,
                        display(&p.name),
                        p.upstream_label(),
                        p.endpoint.host,
                        p.endpoint.port,
                        if p.authenticated {
                            "已配置认证"
                        } else {
                            "无认证"
                        }
                    );
                }
                println!(
                    "配置版本 {}；以上为保存的配置，未检查连通性。",
                    catalog.revision
                );
            }
            return Ok(());
        }
        Command::Create { name, node } => {
            return save_manual(
                root,
                catalog,
                ManualEdit::Create { name },
                node.input(false)?,
                false,
                json,
                &mut crate::foreground::Foreground::new(),
            )
            .await
            .map(|_| ());
        }
        Command::Update {
            id,
            node,
            apply_to_running,
        } => {
            return save_manual(
                root,
                catalog,
                ManualEdit::Update { id },
                node.input(true)?,
                apply_to_running,
                json,
                &mut crate::foreground::Foreground::new(),
            )
            .await
            .map(|_| ());
        }
        Command::Rename { id, name } => ConfigAction::RenameProfile {
            profile_id: id,
            name,
        },
        Command::Remove {
            id,
            apply_to_running,
        } => {
            return remove(
                root,
                catalog.revision,
                id,
                apply_to_running,
                json,
                &mut crate::foreground::Foreground::new(),
            )
            .await;
        }
        Command::Request { .. }
        | Command::Import { .. }
        | Command::Refresh { .. }
        | Command::Nodes { .. }
        | Command::Select { .. } => unreachable!(),
    };
    let (request_id, receipt) = instance_cli::submit(&root, catalog.revision, action, json).await?;
    saved_output(request_id, &receipt, json)
}

pub(crate) enum ManualEdit {
    Create { name: String },
    Update { id: Uuid },
}

pub(crate) async fn save_manual(
    root: PathBuf,
    catalog: crate::configuration::CatalogPage,
    edit: ManualEdit,
    node: ManualProxyInput,
    apply: bool,
    json: bool,
    foreground: &mut crate::foreground::Foreground,
) -> Result<Uuid, Failure> {
    foreground.check()?;
    // Keep reservations until the durable configuration response is known.
    let mut reservations = Vec::new();
    let action = match edit {
        ManualEdit::Create { name } => {
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
            ConfigAction::CreateManualProfile {
                profile_id: Uuid::new_v4(),
                name,
                endpoint: endpoint.ok_or_else(|| fail(3, "LOCAL_PROXY_PORT_UNAVAILABLE"))?,
                node,
            }
        }
        ManualEdit::Update { id } => {
            let snapshot = coordinator::core_status(root.clone())
                .await
                .map_err(|e| fail(3, e.to_string()))?;
            if snapshot.profiles.iter().any(|p| p.id == id) {
                crate::core_cli::prepare_and_apply_with_foreground(
                    root,
                    app_proxy_core::core_control::CoreAction::PrepareUpdate {
                        expected_revision: catalog.revision,
                        profile_id: id,
                        node,
                    },
                    apply,
                    json,
                    foreground,
                )
                .await?;
                return Ok(id);
            }
            ConfigAction::UpdateManualProfile {
                profile_id: id,
                node,
            }
        }
    };
    foreground.check()?;
    let (request_id, receipt) = instance_cli::submit(&root, catalog.revision, action, json).await?;
    drop(reservations);
    saved_output(request_id, &receipt, json)?;
    Ok(receipt.entity_id)
}

fn saved_output(request_id: Uuid, receipt: &ConfigReceipt, json: bool) -> Result<(), Failure> {
    if json {
        print(&serde_json::json!({"request_id":request_id,"receipt":receipt}))
    } else {
        println!(
            "代理配置已保存：{}，版本 {}。",
            receipt.entity_id, receipt.revision
        );
        Ok(())
    }
}

pub(crate) async fn remove(
    root: PathBuf,
    revision: u64,
    id: Uuid,
    apply: bool,
    json: bool,
    foreground: &mut crate::foreground::Foreground,
) -> Result<(), Failure> {
    foreground.check()?;
    let snapshot = coordinator::core_status(root.clone())
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    if snapshot.profiles.iter().any(|p| p.id == id) {
        use app_proxy_windows::core_state::CoreState;
        match snapshot.recorded {
            CoreState::Starting { .. } => {
                return Err(fail(
                    6,
                    "内核创建尚待核对；请先执行 core recover-start，再移除代理。",
                ));
            }
            CoreState::Down { .. } => {
                return Err(fail(
                    3,
                    "内核已退出但保留恢复集合；请先执行 core stop，再移除代理。",
                ));
            }
            _ => {}
        }
        crate::core_cli::prepare_and_apply_with_foreground(
            root,
            app_proxy_core::core_control::CoreAction::PrepareRemove {
                expected_revision: revision,
                profile_id: id,
            },
            apply,
            json,
            foreground,
        )
        .await
    } else {
        let (request_id, receipt) = instance_cli::submit(
            &root,
            revision,
            ConfigAction::RemoveProfile { profile_id: id },
            json,
        )
        .await?;
        saved_output(request_id, &receipt, json)
    }
}
