//! Sticky Ctrl+C intent across dependency repair, prompts and launch continuation.
use crate::exit::{self, Failure, fail};
use tokio::sync::watch;

fn secret_bytes(reader: &mut impl std::io::BufRead, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{BufRead, Read};
    let mut bytes = Vec::new();
    reader
        .take((limit + 3) as u64)
        .read_until(b'\n', &mut bytes)?;
    // Reject an oversized value only after consuming its whole line. Otherwise
    // its secret suffix could become a selection in the next menu prompt.
    if bytes.len() == limit + 3 && !bytes.ends_with(b"\n") {
        loop {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let newline = buffer.iter().position(|b| *b == b'\n');
            let consumed = newline.map_or(buffer.len(), |index| index + 1);
            reader.consume(consumed);
            if newline.is_some() {
                break;
            }
        }
    }
    Ok(bytes)
}

pub(crate) struct Foreground {
    cancelled: watch::Receiver<bool>,
    listener: Option<tokio::task::JoinHandle<()>>,
    quiet: bool,
    allow_console: bool,
    console: Option<app_proxy_windows::console::ForegroundConsole>,
    launch_request: Option<uuid::Uuid>,
    core_request: Option<uuid::Uuid>,
    signal_error: bool,
}
impl Foreground {
    pub async fn read_secret_line(&mut self, limit: usize) -> Result<Option<String>, Failure> {
        use std::io::IsTerminal;
        self.check()?;
        let hidden = if std::io::stdin().is_terminal() {
            Some(
                app_proxy_windows::console::HiddenInput::begin()
                    .map_err(|_| fail(exit::UNAVAILABLE, "SECRET_INPUT_UNAVAILABLE"))?,
            )
        } else {
            None
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let result = secret_bytes(&mut std::io::stdin().lock(), limit);
            let _ = sender.send(result);
        });
        let result = tokio::select! { biased;
            _ = self.cancelled() => Err(fail(exit::ACTION_REQUIRED, "已返回；不会继续操作。")),
            result = receiver => result.map_err(|_| fail(exit::INVALID, "SECRET_INPUT_FAILED"))?.map_err(|_| fail(exit::INVALID, "SECRET_INPUT_FAILED")),
        };
        if hidden.is_some() {
            eprintln!();
        }
        drop(hidden);
        let bytes = result?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let mut text =
            String::from_utf8(bytes).map_err(|_| fail(exit::INVALID, "SECRET_INPUT_INVALID"))?;
        if text.ends_with('\n') {
            text.pop();
            if text.ends_with('\r') {
                text.pop();
            }
        }
        if text.len() > limit || text.contains('\0') {
            return Err(fail(exit::INVALID, "SECRET_INPUT_INVALID"));
        }
        Ok(Some(text))
    }
    pub fn new() -> Self {
        let mut foreground = Self::detached(false);
        foreground.quiet = false;
        let _ = foreground.start_signals();
        foreground
    }
    pub fn detached(allow_console: bool) -> Self {
        let (_, cancelled) = watch::channel(false);
        Self {
            cancelled,
            listener: None,
            quiet: true,
            allow_console,
            console: None,
            launch_request: None,
            core_request: None,
            signal_error: false,
        }
    }
    fn start_signals(&mut self) -> Result<(), Failure> {
        // Register synchronously before exposing any prompt. Merely spawning a
        // future leaves Ctrl+C on AllocConsole's default terminate handler until
        // the task is first polled.
        let mut signal = tokio::signal::windows::ctrl_c().map_err(|_| {
            self.signal_error = true;
            fail(exit::UNAVAILABLE, "CANCEL_LISTENER_UNAVAILABLE")
        })?;
        let (sender, cancelled) = watch::channel(false);
        let listener = tokio::spawn(async move {
            if signal.recv().await.is_some() {
                let _ = sender.send(true);
            }
        });
        self.cancelled = cancelled;
        self.listener = Some(listener);
        Ok(())
    }
    pub fn quiet(&self) -> bool {
        self.quiet
    }
    pub fn can_prompt(&self) -> bool {
        if self.quiet {
            self.allow_console
        } else {
            crate::core_cli::interactive()
        }
    }
    pub fn show_console(&mut self) -> Result<(), Failure> {
        if !self.quiet {
            return self.check();
        }
        if !self.allow_console {
            return Err(fail(exit::ACTION_REQUIRED, "FOREGROUND_REQUIRED"));
        }
        self.console = Some(
            app_proxy_windows::console::ForegroundConsole::open()
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?,
        );
        // Detached launch has never registered Tokio signals, so allocation
        // cannot discard its handler. Register only after the console exists.
        self.start_signals()?;
        self.quiet = false;
        eprintln!("App Proxy：启动需要你的选择。\n{}", self.request_summary());
        Ok(())
    }
    pub fn remember_launch(&mut self, id: uuid::Uuid) {
        self.launch_request = Some(id);
    }
    pub fn remember_core(&mut self, id: uuid::Uuid) {
        self.core_request = Some(id);
    }
    pub fn request_summary(&self) -> String {
        let mut text = String::new();
        if let Some(id) = self.launch_request {
            text.push_str(&format!("启动请求：{id}\n查询：launch inspect {id}\n"));
        }
        if let Some(id) = self.core_request {
            text.push_str(&format!("代理操作：{id}\n查询：core request {id}\n"));
        }
        text
    }
    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }
    pub fn check(&self) -> Result<(), Failure> {
        if self.signal_error {
            return Err(fail(exit::UNAVAILABLE, "CANCEL_LISTENER_UNAVAILABLE"));
        }
        if self.is_cancelled() {
            Err(fail(
                exit::ACTION_REQUIRED,
                "已取消后续操作；已接受的操作请按原编号查询。",
            ))
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
        Ok(self
            .read_optional_line()
            .await?
            .unwrap_or_else(|| "2".into()))
    }
    pub async fn read_optional_line(&mut self) -> Result<Option<String>, Failure> {
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
            _ = self.cancelled() => Err(fail(exit::ACTION_REQUIRED, "已返回；不会继续执行后续操作。")),
            result = receiver => {
                let (bytes, input) = result.map_err(|_| fail(exit::INTERNAL, "PROMPT_READ_FAILED"))?
                    .map_err(|_| fail(exit::INTERNAL, "PROMPT_READ_FAILED"))?;
                if bytes == 0 { Ok(None) } else { Ok(Some(input)) }
            }
        }
    }
}
impl Drop for Foreground {
    fn drop(&mut self) {
        if let Some(listener) = &self.listener {
            listener.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Cursor};

    #[test]
    fn oversized_secret_consumes_only_its_line_with_bounded_storage() {
        let mut input = vec![b'x'; 100_000];
        input.extend_from_slice(b"1\r\n2\n");
        let mut reader = BufReader::with_capacity(7, Cursor::new(input));
        let secret = secret_bytes(&mut reader, 10).unwrap();
        assert_eq!(secret.len(), 13);
        let mut next = String::new();
        reader.read_line(&mut next).unwrap();
        assert_eq!(next, "2\n");
        assert_eq!(
            secret_bytes(&mut Cursor::new(vec![b'x'; 100]), 10)
                .unwrap()
                .len(),
            13
        );
    }

    #[test]
    #[ignore = "native console fixture; launched only by detached_foreground_console_contract"]
    fn detached_foreground_console_child() {
        use std::io::IsTerminal;
        let output = std::path::PathBuf::from(
            std::env::var_os("APP_PROXY_CONSOLE_TEST_OUTPUT").expect("fixture output"),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut foreground = Foreground::detached(true);
            foreground.show_console().unwrap();
            assert!(std::io::stdin().is_terminal());
            assert!(std::io::stdout().is_terminal());
            assert!(std::io::stderr().is_terminal());
            // Current-thread runtime has not polled the spawned signal listener.
            app_proxy_windows::console::test_support::ctrl_c().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
            tokio::time::timeout(std::time::Duration::from_secs(3), foreground.cancelled())
                .await
                .unwrap();
            assert!(foreground.check().is_err());
            assert!(foreground.read_line().await.is_err());
        });
        // Console Drop restored the original redirected standard handles.
        assert!(!std::io::stdout().is_terminal());
        std::fs::write(
            output,
            b"synchronous registration and redirected handles verified",
        )
        .unwrap();
    }

    #[test]
    #[ignore = "creates an isolated native console; explicit desktop validation"]
    fn detached_foreground_console_contract() {
        use std::os::windows::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("result.txt");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "foreground::tests::detached_foreground_console_child",
            ])
            .env("APP_PROXY_CONSOLE_TEST_OUTPUT", &output)
            .creation_flags(0x00000008) // DETACHED_PROCESS: no inherited or headless console
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                assert!(
                    status.success(),
                    "{status:?}\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("native console fixture timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"synchronous registration and redirected handles verified"
        );
    }
}
