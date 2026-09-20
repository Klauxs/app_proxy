//! Fixed elevated host entry point. Sends process hints only, never accepts
//! commands and never launches, terminates, or configures an application.
use crate::{
    Error, Result, etw::EventBatch, event_pipe::EventListener, guard_deployment::Deployment,
    identity, ipc::AcceptError,
};
use std::time::Duration;
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const HEARTBEAT: Duration = Duration::from_millis(20);

pub async fn run(store: Uuid, generation: Uuid) -> Result<()> {
    identity::assert_elevated_user()?;
    let deployment = Deployment::open(store, generation)?;
    if identity::current()?.image_file != *deployment.host_image() {
        return Err(Error::Invalid("GUARD_LISTENER_IMAGE_MISMATCH"));
    }
    // Keep both pins until after the stream and ETW controller are dropped.
    let coordinator = deployment.coordinator()?;
    let mut endpoint = EventListener::bind(store, vec![coordinator.image().clone()])?;
    let mut sender = accept_until(CONNECT_TIMEOUT, async || endpoint.accept().await).await?;
    // No trace is created or recovered before an authenticated ordinary host
    // connects. The journal excludes simultaneous owners across generations.
    let mut trace = deployment.event_trace()?;
    let ready = trace.listener.ready();
    let result = pump(
        |flush| {
            if flush {
                trace.listener.drain()
            } else {
                trace.listener.drain_ready()
            }
        },
        &ready,
        async |batch| sender.send(batch).await,
    )
    .await;
    // A disconnected, stalled or terminated coordinator ends this helper. It
    // cannot leave an unbounded privileged service waiting for future commands.
    let stopped = trace.listener.stop();
    result.and(stopped)
}

async fn accept_until<T>(
    timeout: Duration,
    mut accept: impl AsyncFnMut() -> std::result::Result<T, AcceptError>,
) -> Result<T> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Invalid("EVENT_COORDINATOR_TIMEOUT"));
        }
        match tokio::time::timeout_at(deadline, accept()).await {
            Ok(Ok(peer)) => return Ok(peer),
            Ok(Err(AcceptError::Peer(_))) => tokio::task::yield_now().await,
            Ok(Err(AcceptError::Listener(error))) => return Err(error),
            Err(_) => return Err(Error::Invalid("EVENT_COORDINATOR_TIMEOUT")),
        }
    }
}

async fn pump(
    mut drain: impl FnMut(bool) -> Result<EventBatch>,
    ready: &tokio::sync::Notify,
    mut send: impl AsyncFnMut(EventBatch) -> Result<()>,
) -> Result<()> {
    let mut refresh = tokio::time::interval(HEARTBEAT);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut backlog = false;
    loop {
        // A hot callback queue must not starve ownership/loss checks. Native
        // flushes run on the timer, never for every callback or on its thread.
        let flush = tokio::select! {
            biased;
            _ = refresh.tick() => true,
            _ = ready.notified() => false,
            _ = tokio::task::yield_now(), if backlog => false,
        };
        let batch = drain(flush)?;
        let ended = batch.ended;
        backlog = batch.hints.len() == crate::etw::BATCH_LIMIT;
        send(batch).await?;
        if let Some(code) = ended {
            return if code == 0 {
                Ok(())
            } else {
                Err(Error::Windows {
                    operation: "ProcessTraceEnded",
                    code,
                })
            };
        }
    }
}

#[cfg(test)]
mod tests;
