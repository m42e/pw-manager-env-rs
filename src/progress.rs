use std::io::{self, IsTerminal, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

enum SpinnerCommand {
    UpdateMessage(String),
    Finish(String),
    Clear,
}

struct SpinnerState {
    tx: Sender<SpinnerCommand>,
    handle: thread::JoinHandle<()>,
}

/// Lightweight terminal spinner for long-running CLI operations.
///
/// The spinner writes to stderr and activates only when stderr is a TTY,
/// so shell-evaluable stdout output remains unchanged.
pub struct ActivitySpinner {
    state: Option<SpinnerState>,
}

impl ActivitySpinner {
    pub fn new(initial_message: impl Into<String>) -> Self {
        if cfg!(test) {
            return Self { state: None };
        }

        if !io::stderr().is_terminal() {
            return Self { state: None };
        }

        let (tx, rx) = mpsc::channel();
        let message = initial_message.into();
        let handle = thread::spawn(move || run_spinner_loop(rx, message));

        Self {
            state: Some(SpinnerState { tx, handle }),
        }
    }

    pub fn set_message(&self, message: impl Into<String>) {
        if let Some(state) = &self.state {
            let _ = state.tx.send(SpinnerCommand::UpdateMessage(message.into()));
        }
    }

    pub fn finish(&mut self, message: impl Into<String>) {
        self.stop(SpinnerCommand::Finish(message.into()));
    }

    fn stop(&mut self, command: SpinnerCommand) {
        if let Some(state) = self.state.take() {
            let _ = state.tx.send(command);
            let _ = state.handle.join();
        }
    }
}

impl Drop for ActivitySpinner {
    fn drop(&mut self) {
        self.stop(SpinnerCommand::Clear);
    }
}

fn run_spinner_loop(rx: Receiver<SpinnerCommand>, mut message: String) {
    let frames = ['|', '/', '-', '\\'];
    let mut frame_index = 0usize;
    let mut prev_line_len = 0usize;
    let mut stderr = io::stderr();

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(SpinnerCommand::UpdateMessage(next)) => {
                message = next;
            }
            Ok(SpinnerCommand::Finish(done_message)) => {
                write_status_line(&mut stderr, "ok", &done_message, &mut prev_line_len);
                let _ = writeln!(stderr);
                let _ = stderr.flush();
                return;
            }
            Ok(SpinnerCommand::Clear) => {
                clear_status_line(&mut stderr, prev_line_len);
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let frame = frames[frame_index % frames.len()];
                frame_index = frame_index.wrapping_add(1);
                write_status_line(
                    &mut stderr,
                    &frame.to_string(),
                    &message,
                    &mut prev_line_len,
                );
                let _ = stderr.flush();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                clear_status_line(&mut stderr, prev_line_len);
                return;
            }
        }
    }
}

fn write_status_line(
    stderr: &mut io::Stderr,
    marker: &str,
    message: &str,
    prev_line_len: &mut usize,
) {
    let line = format!("[{marker}] {message}");
    let pad_len = prev_line_len.saturating_sub(line.len());
    let _ = write!(stderr, "\r{line}{}", " ".repeat(pad_len));
    *prev_line_len = line.len();
}

fn clear_status_line(stderr: &mut io::Stderr, prev_line_len: usize) {
    if prev_line_len > 0 {
        let _ = write!(stderr, "\r{}\r", " ".repeat(prev_line_len));
        let _ = stderr.flush();
    }
}
