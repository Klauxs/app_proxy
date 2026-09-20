//! Epoch-scoped read/prepare sessions. Configuration and core mutations still
//! use their existing durable requests; a preview never starts or stops a core.
use crate::{configuration::Configuration, core_manager::CoreManager, subscription_download};
use app_proxy_core::{
    model::{Endpoint, NetworkBinding, ProxySource},
    subscription::{self, Parsed, saved::ProtocolKind},
};
use app_proxy_windows::{
    Error, Result,
    subscription_stage::{ImportRequest, StagedSubscription},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uuid::Uuid;

const LIMIT: usize = 4;
const TTL: Duration = Duration::from_secs(600);
const READY_LIMIT: usize = 8 * 1024 * 1024;
const STAGE_LIMIT: usize = 512 * 1024;

#[cfg(test)]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreviewRequest {
    Import {
        url: String,
        network: NetworkBinding,
    },
    Refresh {
        profile_id: Uuid,
        network: NetworkBinding,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StageRequest {
    Import {
        profile_id: Uuid,
        name: String,
        endpoint: Endpoint,
        selected_names: Vec<String>,
    },
    Refresh {},
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreviewStatus {
    Pending {},
    Ready { nodes: usize, unsupported: usize },
    Failed { code: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSummary {
    pub name: String,
    pub protocol: ProtocolKind,
    pub server: String,
    pub port: u16,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewPage {
    pub status: PreviewStatus,
    pub title: Option<String>,
    pub nodes: Vec<NodeSummary>,
    pub next_offset: Option<usize>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedNodeSummary {
    pub id: Uuid,
    pub node: NodeSummary,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPage {
    pub revision: u64,
    pub source_revision: u64,
    pub selected_node_id: Uuid,
    pub selected_node_ids: Vec<Uuid>,
    pub nodes: Vec<SavedNodeSummary>,
    pub next_offset: Option<usize>,
}

pub fn saved_page(
    manifest: &app_proxy_core::model::Manifest,
    profile_id: Uuid,
    offset: usize,
    expected_revision: Option<u64>,
) -> Result<SavedPage> {
    if expected_revision.is_some_and(|r| r != manifest.revision) {
        return Err(Error::Invalid("CATALOG_CHANGED"));
    }
    let profile = manifest
        .profiles
        .iter()
        .find(|p| p.id == profile_id)
        .ok_or(Error::Invalid("PROFILE_NOT_FOUND"))?;
    let ProxySource::Subscription {
        revision, nodes, ..
    } = &profile.source
    else {
        return Err(Error::Invalid("SUBSCRIPTION_PROFILE_REQUIRED"));
    };
    let end = offset
        .checked_add(64)
        .ok_or(Error::Invalid("INVALID_PREVIEW_OFFSET"))?;
    Ok(SavedPage {
        revision: manifest.revision,
        source_revision: *revision,
        selected_node_id: profile.selected_node_id,
        selected_node_ids: profile.selected_node_ids(),
        nodes: nodes
            .iter()
            .skip(offset)
            .take(64)
            .map(|n| SavedNodeSummary {
                id: n.id,
                node: NodeSummary {
                    name: n.name.clone(),
                    protocol: n.protocol,
                    server: n.server.clone(),
                    port: n.port,
                },
            })
            .collect(),
        next_offset: (end < nodes.len()).then_some(end),
    })
}

enum Source {
    Import {
        url: String,
    },
    Refresh {
        profile_id: Uuid,
        revision: u64,
        url_secret_id: Uuid,
    },
}
struct Ready {
    source: Source,
    parsed: Parsed,
    title: Option<String>,
}
enum State {
    Pending,
    Ready(Ready),
    Failed(String),
}
struct Stage {
    id: Uuid,
    digest: [u8; 32],
    result: Option<std::result::Result<Vec<u8>, &'static str>>,
}
struct Entry {
    digest: [u8; 32],
    expires: Instant,
    cancel: watch::Sender<bool>,
    closing: bool,
    state: State,
    stage: Option<Stage>,
}
impl Entry {
    fn status(&self) -> PreviewStatus {
        match &self.state {
            State::Pending => PreviewStatus::Pending {},
            State::Ready(ready) => PreviewStatus::Ready {
                nodes: ready.parsed.nodes.len(),
                unsupported: ready.parsed.unsupported.len(),
            },
            State::Failed(code) => PreviewStatus::Failed { code: code.clone() },
        }
    }
    fn busy(&self) -> bool {
        matches!(self.state, State::Pending)
            || self
                .stage
                .as_ref()
                .is_some_and(|stage| stage.result.is_none())
    }
}

pub struct PreviewService {
    configuration: Arc<Configuration>,
    core: Arc<CoreManager>,
    entries: Mutex<HashMap<Uuid, Entry>>,
}
impl PreviewService {
    pub fn new(configuration: Arc<Configuration>, core: Arc<CoreManager>) -> Arc<Self> {
        Arc::new(Self {
            configuration,
            core,
            entries: Mutex::new(HashMap::new()),
        })
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<Uuid, Entry>>> {
        self.entries
            .lock()
            .map_err(|_| Error::Invalid("SUBSCRIPTION_PREVIEW_FAILED"))
    }
    pub fn keeps_alive(&self) -> Result<bool> {
        let mut entries = self.lock()?;
        prune(&mut entries);
        Ok(!entries.is_empty())
    }

    pub fn begin(self: &Arc<Self>, id: Uuid, request: PreviewRequest) -> Result<PreviewStatus> {
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_PREVIEW_ID"));
        }
        match &request {
            PreviewRequest::Import { url, .. } => {
                subscription::source_url(url)
                    .map_err(|_| Error::Invalid("SUBSCRIPTION_URL_INVALID"))?;
            }
            PreviewRequest::Refresh { profile_id, .. } if profile_id.is_nil() => {
                return Err(Error::Invalid("PROFILE_NOT_FOUND"));
            }
            _ => {}
        }
        let bytes = serde_json::to_vec(&request)?;
        if bytes.len() > 32768 {
            return Err(Error::Invalid("SUBSCRIPTION_REQUEST_TOO_LARGE"));
        }
        let digest = Sha256::digest(bytes).into();
        let (sender, receiver) = watch::channel(false);
        let mut entries = self.lock()?;
        prune(&mut entries);
        if let Some(entry) = entries.get(&id) {
            if entry.closing || entry.expires <= Instant::now() {
                return Err(Error::Invalid("SUBSCRIPTION_PREVIEW_EXPIRED"));
            }
            return if entry.digest == digest {
                Ok(entry.status())
            } else {
                Err(Error::Invalid("REQUEST_ID_CONFLICT"))
            };
        }
        if entries.len() >= LIMIT {
            return Err(Error::Invalid("SUBSCRIPTION_PREVIEW_LIMIT"));
        }
        entries.insert(
            id,
            Entry {
                digest,
                expires: Instant::now() + TTL,
                cancel: sender,
                closing: false,
                state: State::Pending,
                stage: None,
            },
        );
        drop(entries);
        let service = self.clone();
        let runtime = tokio::runtime::Handle::current();
        // Keep the slot until actual parsing/native store work finishes, even if
        // cancellation arrives while that work cannot be interrupted.
        tokio::task::spawn_blocking(move || {
            let outcome = runtime.block_on(service.download(request, receiver));
            if let Ok(mut entries) = service.lock() {
                if let Some(entry) = entries.get_mut(&id) {
                    entry.state = match outcome {
                        Ok(ready) => State::Ready(ready),
                        Err(code) => State::Failed(code),
                    };
                }
                prune(&mut entries);
            }
        });
        Ok(PreviewStatus::Pending {})
    }

    async fn download(
        &self,
        request: PreviewRequest,
        mut cancelled: watch::Receiver<bool>,
    ) -> std::result::Result<Ready, String> {
        let (source, url, network) = match request {
            PreviewRequest::Import { url, network } => {
                (Source::Import { url: url.clone() }, url, network)
            }
            PreviewRequest::Refresh {
                profile_id,
                network,
            } => {
                let store = self.configuration.lock().map_err(safe)?;
                let manifest = store.load().map_err(safe)?;
                let profile = manifest
                    .profiles
                    .iter()
                    .find(|p| p.id == profile_id)
                    .ok_or("PROFILE_NOT_FOUND")?;
                let ProxySource::Subscription {
                    revision,
                    url_secret_id,
                    ..
                } = &profile.source
                else {
                    return Err("SUBSCRIPTION_PROFILE_REQUIRED".into());
                };
                (
                    Source::Refresh {
                        profile_id,
                        revision: *revision,
                        url_secret_id: *url_secret_id,
                    },
                    store.read_secret(*url_secret_id).map_err(safe)?,
                    network,
                )
            }
        };
        if *cancelled.borrow() {
            return Err("SUBSCRIPTION_PREVIEW_CANCELLED".into());
        }
        let downloaded = tokio::select! {
            biased;
            _ = cancelled.wait_for(|value| *value) => return Err("SUBSCRIPTION_PREVIEW_CANCELLED".into()),
            result = async {
                match network {
                    NetworkBinding::Direct {} => subscription_download::download(&url, None).await.map_err(|e| e.to_string()),
                    NetworkBinding::Profile { profile_id } => self.core.download_subscription(profile_id, &url).await.map_err(safe)?.map_err(|e| e.to_string()),
                }
            } => result?,
        };
        let parsed = subscription::parse(downloaded.text()).map_err(|e| e.to_string())?;
        if parsed.nodes.is_empty() {
            return Err("SUBSCRIPTION_NO_SUPPORTED_NODES".into());
        }
        let mut size = 0usize;
        for node in &parsed.nodes {
            let (_, encoded) =
                subscription::saved::SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), node)
                    .map_err(|e| e.to_string())?;
            size = size.saturating_add(encoded.len());
            if size > READY_LIMIT {
                return Err("SUBSCRIPTION_PREVIEW_TOO_LARGE".into());
            }
        }
        if *cancelled.borrow() {
            return Err("SUBSCRIPTION_PREVIEW_CANCELLED".into());
        }
        Ok(Ready {
            source,
            parsed,
            title: downloaded.title().map(str::to_owned),
        })
    }

    pub fn page(&self, id: Uuid, offset: usize) -> Result<PreviewPage> {
        let mut entries = self.lock()?;
        prune(&mut entries);
        let entry = entries
            .get(&id)
            .filter(|e| !e.closing && e.expires > Instant::now())
            .ok_or(Error::Invalid("SUBSCRIPTION_PREVIEW_EXPIRED"))?;
        let end = offset
            .checked_add(64)
            .ok_or(Error::Invalid("INVALID_PREVIEW_OFFSET"))?;
        let (nodes, next_offset) = if let State::Ready(ready) = &entry.state {
            (
                ready
                    .parsed
                    .nodes
                    .iter()
                    .skip(offset)
                    .take(64)
                    .map(|n| NodeSummary {
                        name: n.name.clone(),
                        protocol: ProtocolKind::of(&n.protocol),
                        server: n.server.clone(),
                        port: n.port,
                    })
                    .collect(),
                (end < ready.parsed.nodes.len()).then_some(end),
            )
        } else {
            (Vec::new(), None)
        };
        Ok(PreviewPage {
            status: entry.status(),
            title: match &entry.state {
                State::Ready(ready) => ready.title.clone(),
                _ => None,
            },
            nodes,
            next_offset,
        })
    }

    pub fn close(&self, id: Uuid) -> Result<()> {
        let mut entries = self.lock()?;
        if let Some(entry) = entries.get_mut(&id) {
            entry.closing = true;
            let _ = entry.cancel.send(true);
        }
        prune(&mut entries);
        Ok(())
    }

    pub fn stage(
        &self,
        preview_id: Uuid,
        request_id: Uuid,
        request: StageRequest,
    ) -> Result<StagedSubscription> {
        if request_id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        let bytes = serde_json::to_vec(&request)?;
        if bytes.len() > 32768 {
            return Err(Error::Invalid("SUBSCRIPTION_REQUEST_TOO_LARGE"));
        }
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        let (source, parsed) = {
            let mut entries = self.lock()?;
            prune(&mut entries);
            let entry = entries
                .get_mut(&preview_id)
                .filter(|e| !e.closing && e.expires > Instant::now())
                .ok_or(Error::Invalid("SUBSCRIPTION_PREVIEW_EXPIRED"))?;
            if let Some(stage) = &entry.stage {
                if stage.id != request_id || stage.digest != digest {
                    return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
                }
                return match &stage.result {
                    Some(Ok(bytes)) => Ok(serde_json::from_slice(bytes)?),
                    Some(Err(code)) => Err(Error::Invalid(code)),
                    None => Err(Error::Invalid("SUBSCRIPTION_STAGE_PENDING")),
                };
            }
            let State::Ready(ready) = &entry.state else {
                return Err(Error::Invalid("SUBSCRIPTION_PREVIEW_NOT_READY"));
            };
            let source = match &ready.source {
                Source::Import { url } => Source::Import { url: url.clone() },
                Source::Refresh {
                    profile_id,
                    revision,
                    url_secret_id,
                } => Source::Refresh {
                    profile_id: *profile_id,
                    revision: *revision,
                    url_secret_id: *url_secret_id,
                },
            };
            let parsed = ready.parsed.clone();
            entry.stage = Some(Stage {
                id: request_id,
                digest,
                result: None,
            });
            (source, parsed)
        };
        let result = (|| {
            let mut store = self.configuration.lock()?;
            let staged = match (source, request) {
                (
                    Source::Import { url },
                    StageRequest::Import {
                        profile_id,
                        name,
                        endpoint,
                        selected_names,
                    },
                ) => {
                    let expected_revision = store.load()?.revision;
                    store.stage_subscription_import(
                        &ImportRequest {
                            request_id,
                            expected_revision,
                            profile_id,
                            name,
                            endpoint,
                            url,
                            selected_names,
                        },
                        &parsed,
                    )?
                }
                (
                    Source::Refresh {
                        profile_id,
                        revision,
                        url_secret_id,
                    },
                    StageRequest::Refresh {},
                ) => store.stage_subscription_refresh(
                    request_id,
                    profile_id,
                    revision,
                    url_secret_id,
                    &parsed,
                )?,
                _ => return Err(Error::Invalid("SUBSCRIPTION_PREVIEW_KIND_MISMATCH")),
            };
            let encoded = serde_json::to_vec(&staged)?;
            if encoded.len() > STAGE_LIMIT {
                return Err(Error::Invalid("SUBSCRIPTION_STAGE_TOO_LARGE"));
            }
            Ok((staged, encoded))
        })();
        let mut entries = self.lock()?;
        if let Some(entry) = entries.get_mut(&preview_id)
            && let Some(stage) = &mut entry.stage
        {
            stage.result = Some(match &result {
                Ok((_, bytes)) => Ok(bytes.clone()),
                Err(Error::Invalid(code)) => Err(code),
                Err(_) => Err("SUBSCRIPTION_STAGE_FAILED"),
            });
        }
        prune(&mut entries);
        result.map(|(staged, _)| staged)
    }
}

fn prune(entries: &mut HashMap<Uuid, Entry>) {
    let now = Instant::now();
    entries.retain(|_, entry| {
        if entry.closing || entry.expires <= now {
            let _ = entry.cancel.send(true);
            entry.busy()
        } else {
            true
        }
    });
}
fn safe(error: Error) -> String {
    match error {
        Error::Invalid(code) => code.into(),
        _ => "SUBSCRIPTION_PREVIEW_FAILED".into(),
    }
}
