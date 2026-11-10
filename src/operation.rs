use std::{
    collections::VecDeque,
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use unicode_width::UnicodeWidthStr;

use crate::git::{Operation, Repository, safe_text};

// Bound both queued output and retained history so noisy hooks cannot exhaust memory.
const QUEUED_CHUNKS: usize = 64;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_LINES: usize = 2000;
const MAX_LINE_BYTES: usize = 8 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum OperationResult {
    Success,
    Cancelled,
    Failed(String),
}

pub enum OperationEvent {
    Prepared(String),
    Output(String),
    Finished(OperationResult),
}

pub struct OperationHandle {
    pub events: Receiver<OperationEvent>,
    pub cancel: Sender<()>,
}

pub fn spawn(repo: Repository, operation: Operation) -> OperationHandle {
    let (sender, events) = mpsc::sync_channel(QUEUED_CHUNKS);
    let (cancel, cancellation) = mpsc::channel();
    std::thread::spawn(move || {
        let result = repo
            .operation_command(&operation)
            .and_then(|prepared| {
                let _ = sender.send(OperationEvent::Prepared(prepared.success_message));
                if let Some(preparation) = prepared.preparation {
                    let result = run(preparation, None, &sender, &cancellation)?;
                    if result != OperationResult::Success {
                        return Ok(result);
                    }
                }
                run(prepared.command, prepared.input, &sender, &cancellation)
            })
            .unwrap_or_else(|error| OperationResult::Failed(format!("{error:#}")));
        let _ = sender.send(OperationEvent::Finished(result));
    });
    OperationHandle { events, cancel }
}

fn run(
    mut command: Command,
    input: Option<String>,
    sender: &SyncSender<OperationEvent>,
    cancellation: &Receiver<()>,
) -> Result<OperationResult> {
    if !matches!(cancellation.try_recv(), Err(mpsc::TryRecvError::Empty)) {
        return Ok(OperationResult::Cancelled);
    }
    // Give Git and its hooks/transports a separate group, never signal the TUI.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
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
        let mut status = None;
        let mut interrupted = None;
        let mut forced = false;
        loop {
            if status.is_none() {
                status = child.try_wait().context("Could not wait for Git")?;
            }
            // Descendants may still hold the pipes even after Git itself exits.
            if status.is_some()
                && stdout.is_finished()
                && stderr.is_finished()
                && write.is_finished()
                && (interrupted.is_none() || forced || !group_running(&child))
            {
                break;
            }
            if let Some(start) = interrupted {
                if !forced && Instant::now().duration_since(start) >= Duration::from_secs(1) {
                    terminate(&mut child, true).context("Could not stop Git")?;
                    forced = true;
                }
                std::thread::sleep(Duration::from_millis(20));
            } else if !matches!(
                cancellation.recv_timeout(Duration::from_millis(20)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                terminate(&mut child, false).context("Could not interrupt Git")?;
                interrupted = Some(Instant::now());
            }
        }
        let out = stdout
            .join()
            .map_err(|_| anyhow::anyhow!("Git stdout reader panicked"))?;
        let err = stderr
            .join()
            .map_err(|_| anyhow::anyhow!("Git stderr reader panicked"))?;
        let write = write
            .join()
            .map_err(|_| anyhow::anyhow!("Git input writer panicked"))?;
        if interrupted.is_some() {
            return Ok(OperationResult::Cancelled);
        }
        let status = status.unwrap();
        out.context("Could not read Git stdout")?;
        err.context("Could not read Git stderr")?;
        // Prefer Git's failure over a broken pipe when it rejected the input early.
        if !status.success() {
            bail!("Git exited with {status}");
        }
        write.context("Could not send input to Git")?;
        Ok(OperationResult::Success)
    })
}

fn group_running(child: &Child) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks the private process group's existence.
        unsafe { libc::kill(-(child.id() as libc::pid_t), 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = child;
        false
    }
}

fn terminate(child: &mut Child, force: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let signal = if force { libc::SIGKILL } else { libc::SIGINT };
        // SAFETY: a negative child PID addresses the group created by process_group(0).
        let result = unsafe { libc::kill(-(child.id() as libc::pid_t), signal) };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let _ = force;
        let status = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() && child.try_wait()?.is_none() {
            return Err(std::io::Error::other(
                "Could not terminate Git process tree",
            ));
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = force;
        child.kill()
    }
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
    pub result: Option<OperationResult>,
    pub cancelling: bool,
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
            cancelling: false,
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

    pub fn finish(&mut self, result: OperationResult) {
        match &result {
            OperationResult::Failed(error) => self.append(&format!("\n{error}\n")),
            OperationResult::Cancelled => self.append("\nGit operation cancelled\n"),
            OperationResult::Success => {}
        }
        self.cancelling = false;
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
        let (_cancel, cancellation) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            assert_eq!(
                run(command, Some("m".repeat(524288)), &sender, &cancellation).unwrap(),
                OperationResult::Success
            );
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
    fn cancellation_before_start_does_not_run_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("not-created");
        let mut command = Command::new("git");
        command.arg("init").arg(&target);
        let (sender, _events) = mpsc::sync_channel(QUEUED_CHUNKS);
        let (cancel, cancellation) = mpsc::channel();
        cancel.send(()).unwrap();
        assert_eq!(
            run(command, None, &sender, &cancellation).unwrap(),
            OperationResult::Cancelled
        );
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_force_stops_children_that_ignore_interrupts_and_unblocks_input() {
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' INT; sleep 30 & printf ready; wait"]);
        let (sender, events) = mpsc::sync_channel(QUEUED_CHUNKS);
        let (cancel, cancellation) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = run(command, Some("m".repeat(524288)), &sender, &cancellation).unwrap();
            sender.send(OperationEvent::Finished(result)).unwrap();
        });
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            OperationEvent::Output(text) if text == "ready"
        ));
        cancel.send(()).unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            OperationEvent::Finished(OperationResult::Cancelled)
        ));
        worker.join().unwrap();
    }

    #[test]
    fn spawn_failure_is_reported() {
        let (sender, _receiver) = mpsc::sync_channel(QUEUED_CHUNKS);
        let (_cancel, cancellation) = mpsc::channel();
        let error = run(
            Command::new("/nonexistent/diffy-test-command"),
            None,
            &sender,
            &cancellation,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Could not run Git"));
    }
}
