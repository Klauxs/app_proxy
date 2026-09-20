//! Sticky Ctrl+C intent across dependency repair, prompts and launch continuation.
use crate::instance_cli::{Failure, fail};
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
    listener: tokio::task::JoinHandle<()>,
}
impl Foreground {
    pub async fn read_secret_line(&mut self, limit: usize) -> Result<Option<String>, Failure> {
        use std::io::IsTerminal;
        self.check()?;
        let hidden = if std::io::stdin().is_terminal() {
            Some(
                app_proxy_windows::console::HiddenInput::begin()
                    .map_err(|_| fail(3, "SECRET_INPUT_UNAVAILABLE"))?,
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
            _ = self.cancelled() => Err(fail(5, "已返回；不会继续操作。")),
            result = receiver => result.map_err(|_| fail(2, "SECRET_INPUT_FAILED"))?.map_err(|_| fail(2, "SECRET_INPUT_FAILED")),
        };
        if hidden.is_some() {
            eprintln!();
        }
        drop(hidden);
        let bytes = result?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let mut text = String::from_utf8(bytes).map_err(|_| fail(2, "SECRET_INPUT_INVALID"))?;
        if text.ends_with('\n') {
            text.pop();
            if text.ends_with('\r') {
                text.pop();
            }
        }
        if text.len() > limit || text.contains('\0') {
            return Err(fail(2, "SECRET_INPUT_INVALID"));
        }
        Ok(Some(text))
    }
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
            _ = self.cancelled() => Err(fail(5, "已返回；不会继续执行后续操作。")),
            result = receiver => {
                let (bytes, input) = result.map_err(|_| fail(10, "PROMPT_READ_FAILED"))?
                    .map_err(|_| fail(10, "PROMPT_READ_FAILED"))?;
                if bytes == 0 { Ok(None) } else { Ok(Some(input)) }
            }
        }
    }
}
impl Drop for Foreground {
    fn drop(&mut self) {
        self.listener.abort();
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
}
