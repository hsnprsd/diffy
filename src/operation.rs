use std::{
    collections::VecDeque,
    io::{Read, Write},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
};

use anyhow::{Context, Result, bail};
use unicode_width::UnicodeWidthStr;

use crate::git::{Operation, Repository, safe_text};

// Bound both queued output and retained history so noisy hooks cannot exhaust memory.
const QUEUED_CHUNKS: usize = 64;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_LINES: usize = 2000;
const MAX_LINE_BYTES: usize = 8 * 1024;

pub enum OperationEvent {
    Prepared(String),
    Output(String),
    Finished(Result<(), String>),
}

pub fn spawn(repo: Repository, operation: Operation) -> Receiver<OperationEvent> {
    let (sender, receiver) = mpsc::sync_channel(QUEUED_CHUNKS);
    std::thread::spawn(move || {
        let result = repo
            .operation_command(&operation)
            .and_then(|prepared| {
                let _ = sender.send(OperationEvent::Prepared(prepared.success_message));
                run(prepared.command, prepared.input, &sender)
            })
            .map_err(|error| format!("{error:#}"));
        let _ = sender.send(OperationEvent::Finished(result));
    });
    receiver
}

fn run(
    mut command: Command,
    input: Option<String>,
    sender: &SyncSender<OperationEvent>,
) -> Result<()> {
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not run Git")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdin = child.stdin.take();
    std::thread::scope(|scope| {
        let stdout = scope.spawn(|| stream(stdout, sender));
        let stderr = scope.spawn(|| stream(stderr, sender));
        // Writing concurrently also handles large commit messages without blocking output.
        let write = scope.spawn(move || -> std::io::Result<()> {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin.write_all(input.as_bytes())?;
            }
            Ok(())
        });
        let status = child.wait().context("Could not wait for Git");
        let out = stdout
            .join()
            .map_err(|_| anyhow::anyhow!("Git stdout reader panicked"))?;
        let err = stderr
            .join()
            .map_err(|_| anyhow::anyhow!("Git stderr reader panicked"))?;
        let write = write
            .join()
            .map_err(|_| anyhow::anyhow!("Git input writer panicked"))?;
        let status = status?;
        out.context("Could not read Git stdout")?;
        err.context("Could not read Git stderr")?;
        // Prefer Git's failure over a broken pipe when it rejected the input early.
        if !status.success() {
            bail!("Git exited with {status}");
        }
        write.context("Could not send input to Git")?;
        Ok(())
    })
}

fn stream(mut pipe: impl Read, sender: &SyncSender<OperationEvent>) -> std::io::Result<()> {
    let mut buffer = [0; 4096];
    let mut pending = Vec::new();
    loop {
        let count = match pipe.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        pending.extend_from_slice(&buffer[..count]);
        let mut text = String::new();
        let mut consumed = 0;
        while consumed < pending.len() {
            match std::str::from_utf8(&pending[consumed..]) {
                Ok(rest) => {
                    text.push_str(rest);
                    consumed = pending.len();
                }
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    text.push_str(std::str::from_utf8(&pending[consumed..end]).unwrap());
                    consumed = end;
                    if let Some(length) = error.error_len() {
                        text.push('\u{fffd}');
                        consumed += length;
                    } else {
                        break; // Keep incomplete UTF-8 until the next read.
                    }
                }
            }
        }
        pending.drain(..consumed);
        if count == 0 && !pending.is_empty() {
            text.push_str(&String::from_utf8_lossy(&pending));
        }
        // Keep draining even if the UI has exited, so Git cannot block on a full pipe.
        if !text.is_empty() {
            let _ = sender.send(OperationEvent::Output(text));
        }
        if count == 0 {
            return Ok(());
        }
    }
}

pub struct OperationDetails {
    pub label: &'static str,
    pub commit: bool,
    pub success_message: String,
    pub result: Option<Result<(), String>>,
    pub lines: VecDeque<String>,
    bytes: usize,
    carriage_return: bool,
    pub omitted: bool,
    pub scroll: usize,
    pub horizontal: usize,
    pub follow: bool,
    pub viewport_height: usize,
    pub viewport_width: usize,
}

impl OperationDetails {
    pub fn new(operation: &Operation) -> Self {
        Self {
            label: operation.label(),
            commit: matches!(operation, Operation::Commit(_)),
            success_message: String::new(),
            result: None,
            lines: VecDeque::from([String::new()]),
            bytes: 0,
            carriage_return: false,
            omitted: false,
            scroll: 0,
            horizontal: 0,
            follow: true,
            viewport_height: 1,
            viewport_width: 80,
        }
    }

    pub fn finish(&mut self, result: Result<(), String>) {
        if let Err(error) = &result {
            self.append(&format!("\n{error}\n"));
        }
        self.result = Some(result);
    }

    pub fn append(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\r' {
                self.carriage_return = true;
                continue;
            }
            if ch == '\n' {
                self.lines.push_back(String::new());
            } else {
                let line = self.lines.back_mut().unwrap();
                if self.carriage_return {
                    self.bytes -= line.len();
                    line.clear();
                }
                if line.len() + ch.len_utf8() <= MAX_LINE_BYTES {
                    line.push(ch);
                    self.bytes += ch.len_utf8();
                } else {
                    self.omitted = true;
                }
            }
            self.carriage_return = false;
            while self.lines.len() > MAX_OUTPUT_LINES || self.bytes > MAX_OUTPUT_BYTES {
                self.bytes -= self.lines.pop_front().unwrap().len();
                self.scroll = self.scroll.saturating_sub(1);
                self.omitted = true;
            }
        }
        self.clamp_scroll();
    }

    pub fn summary(&self, last: bool) -> &str {
        let mut lines = self.lines.iter().filter(|line| !line.trim().is_empty());
        if last {
            lines.next_back()
        } else {
            lines.next()
        }
        .map_or("No output", String::as_str)
    }

    pub fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.viewport_height.max(1))
    }

    pub fn max_horizontal(&self) -> usize {
        self.lines
            .iter()
            .map(|line| safe_text(line).width())
            .max()
            .unwrap_or(0)
            .saturating_sub(self.viewport_width)
    }

    pub fn clamp_scroll(&mut self) {
        self.scroll = if self.follow {
            self.max_scroll()
        } else {
            self.scroll.min(self.max_scroll())
        };
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll());
        self.follow = delta > 0 && self.scroll == self.max_scroll();
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::Duration;

    use super::*;

    #[test]
    fn progress_updates_replace_lines_and_preserve_crlf_and_partial_output() {
        let mut details = OperationDetails::new(&Operation::Push);
        details.append("Counting: 1%");
        details.append("\rCounting: 50%\r");
        details.append("Counting: 100%\r");
        details.append("\nDone 界");
        assert_eq!(details.lines, ["Counting: 100%", "Done 界"]);
        details.append("\n");
        assert_eq!(details.summary(false), "Counting: 100%");
        assert_eq!(details.summary(true), "Done 界");
    }

    #[test]
    fn history_is_bounded_for_many_lines_long_lines_and_large_output() {
        let mut details = OperationDetails::new(&Operation::Pull);
        details.append(&"row\n".repeat(MAX_OUTPUT_LINES * 2));
        assert_eq!(details.lines.len(), MAX_OUTPUT_LINES);
        assert!(details.omitted);
        details.append(&"界".repeat(MAX_LINE_BYTES));
        assert!(details.lines.back().unwrap().len() <= MAX_LINE_BYTES);
        details.append(&format!("\n{}", "x".repeat(1024)).repeat(1000));
        assert!(details.bytes <= MAX_OUTPUT_BYTES);
        assert_eq!(
            details.bytes,
            details.lines.iter().map(String::len).sum::<usize>()
        );
        assert!(details.lines.len() <= MAX_OUTPUT_LINES);
    }

    #[test]
    fn scrolling_pauses_following_until_returning_to_the_bottom() {
        let mut details = OperationDetails::new(&Operation::Pull);
        details.viewport_height = 3;
        details.append("1\n2\n3\n4\n5");
        assert_eq!(details.scroll, 2);
        details.scroll_by(-1);
        assert!(!details.follow);
        details.append("\n6");
        assert_eq!(details.scroll, 1);
        details.scroll_by(isize::MAX);
        assert!(details.follow);
        assert_eq!(details.scroll, 3);
        details.viewport_height = 20;
        details.clamp_scroll();
        assert_eq!(details.scroll, 0);
    }

    struct ByteReader(std::io::Cursor<Vec<u8>>);

    impl Read for ByteReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(&mut buffer[..1])
        }
    }

    #[test]
    fn output_decoder_preserves_split_utf8_and_replaces_invalid_or_incomplete_bytes() {
        let bytes = ["a界é".as_bytes(), &[0xff, b'b', 0xe2, 0x82]].concat();
        let (sender, receiver) = mpsc::sync_channel(64);
        stream(ByteReader(std::io::Cursor::new(bytes)), &sender).unwrap();
        drop(sender);
        let text: String = receiver
            .into_iter()
            .map(|event| match event {
                OperationEvent::Output(text) => text,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(text, "a界é\u{fffd}b\u{fffd}");
    }

    #[cfg(unix)]
    #[test]
    fn command_drains_both_pipes_while_writing_large_input() {
        let (sender, receiver) = mpsc::sync_channel(QUEUED_CHUNKS);
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "head -c 524288 /dev/zero; head -c 524288 /dev/zero >&2; cat >/dev/null; printf done",
        ]);
        let worker = std::thread::spawn(move || {
            run(command, Some("m".repeat(524288)), &sender).unwrap();
        });
        let mut bytes = 0;
        let mut text_output = String::new();
        loop {
            match receiver.recv_timeout(Duration::from_secs(5)) {
                Ok(OperationEvent::Output(text)) => {
                    bytes += text.len();
                    text_output.extend(text.chars().filter(|&ch| ch != '\0'));
                }
                Ok(_) => unreachable!(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("Git pipes stopped draining"),
            }
        }
        worker.join().unwrap();
        assert_eq!(bytes, 1048576 + 4);
        assert_eq!(text_output, "done");
    }

    #[test]
    fn spawn_failure_is_reported() {
        let (sender, _receiver) = mpsc::sync_channel(QUEUED_CHUNKS);
        let error = run(
            Command::new("/nonexistent/diffy-test-command"),
            None,
            &sender,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Could not run Git"));
    }
}
