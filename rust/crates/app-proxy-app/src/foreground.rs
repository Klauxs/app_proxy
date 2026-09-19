//! Sticky Ctrl+C intent across dependency repair, prompts and launch continuation.
use crate::instance_cli::{Failure, fail};
use tokio::sync::watch;

pub(crate) struct Foreground {
    cancelled: watch::Receiver<bool>,
    listener: tokio::task::JoinHandle<()>,
}
impl Foreground {
    pub fn new() -> Self {
        let (sender, cancelled) = watch::channel(false);
        let listener = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = sender.send(true);
            }
        });
        Self {
            cancelled,
            listener,
        }
    }
    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }
    pub fn check(&self) -> Result<(), Failure> {
        if self.is_cancelled() {
            Err(fail(5, "已取消后续操作；已接受的操作请按原编号查询。"))
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&mut self) {
        if self.cancelled.wait_for(|value| *value).await.is_err() {
            // An unavailable signal listener must not fabricate cancellation.
            std::future::pending::<()>().await;
        }
    }
    pub async fn read_line(&mut self) -> Result<String, Failure> {
        self.check()?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // Unlike spawn_blocking, a pending console read on this thread cannot
        // hold Tokio runtime shutdown after Ctrl+C. This CLI then exits.
        std::thread::spawn(move || {
            let mut input = String::new();
            let result = std::io::stdin()
                .read_line(&mut input)
                .map(|bytes| (bytes, input));
            let _ = sender.send(result);
        });
        tokio::select! {
            biased;
            _ = self.cancelled() => Err(fail(5, "已返回；不会继续执行后续操作。")),
            result = receiver => {
                let (bytes, input) = result.map_err(|_| fail(10, "PROMPT_READ_FAILED"))?
                    .map_err(|_| fail(10, "PROMPT_READ_FAILED"))?;
                if bytes == 0 { Ok("2".into()) } else { Ok(input) }
            }
        }
    }
}
impl Drop for Foreground {
    fn drop(&mut self) {
        self.listener.abort();
    }
}
