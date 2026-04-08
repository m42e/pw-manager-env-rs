use std::io::{self, IsTerminal, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Condvar, Mutex, OnceLock};
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

#[derive(Default)]
struct TerminalOutputState {
    paused_count: usize,
    visible_status_line_len: usize,
}

struct TerminalOutputCoordinator {
    state: Mutex<TerminalOutputState>,
    resume_condvar: Condvar,
}

impl Default for TerminalOutputCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(TerminalOutputState::default()),
            resume_condvar: Condvar::new(),
        }
    }
}

pub struct ProgressOutputSuspension {
    active: bool,
}

impl Drop for ProgressOutputSuspension {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        let coordinator = terminal_output_coordinator();
        let mut state = coordinator.state.lock().unwrap();
        if state.paused_count > 0 {
            state.paused_count -= 1;
            if state.paused_count == 0 {
                coordinator.resume_condvar.notify_all();
            }
        }
    }
}

pub fn suspend_progress_output() -> ProgressOutputSuspension {
    let mut stderr = io::stderr();
    suspend_progress_output_with_writer(&mut stderr)
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

fn terminal_output_coordinator() -> &'static TerminalOutputCoordinator {
    static COORDINATOR: OnceLock<TerminalOutputCoordinator> = OnceLock::new();
    COORDINATOR.get_or_init(TerminalOutputCoordinator::default)
}

fn suspend_progress_output_with_writer<W: Write>(writer: &mut W) -> ProgressOutputSuspension {
    let coordinator = terminal_output_coordinator();
    let mut state = coordinator.state.lock().unwrap();
    state.paused_count += 1;
    clear_status_line(writer, &mut state.visible_status_line_len);
    let _ = writer.flush();

    ProgressOutputSuspension { active: true }
}

fn with_terminal_output_lock<W: Write, R>(
    writer: &mut W,
    f: impl FnOnce(&mut W, &mut TerminalOutputState) -> R,
) -> R {
    let coordinator = terminal_output_coordinator();
    let mut state = coordinator.state.lock().unwrap();
    while state.paused_count > 0 {
        state = coordinator.resume_condvar.wait(state).unwrap();
    }

    f(writer, &mut state)
}

fn run_spinner_loop(rx: Receiver<SpinnerCommand>, mut message: String) {
    let frames = ['|', '/', '-', '\\'];
    let mut frame_index = 0usize;
    let mut stderr = io::stderr();

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(SpinnerCommand::UpdateMessage(next)) => {
                message = next;
            }
            Ok(SpinnerCommand::Finish(done_message)) => {
                with_terminal_output_lock(&mut stderr, |stderr, state| {
                    write_status_line(
                        stderr,
                        "ok",
                        &done_message,
                        &mut state.visible_status_line_len,
                    );
                    let _ = writeln!(stderr);
                    state.visible_status_line_len = 0;
                    let _ = stderr.flush();
                });
                return;
            }
            Ok(SpinnerCommand::Clear) => {
                with_terminal_output_lock(&mut stderr, |stderr, state| {
                    clear_status_line(stderr, &mut state.visible_status_line_len);
                    let _ = stderr.flush();
                });
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let frame = frames[frame_index % frames.len()].to_string();
                frame_index = frame_index.wrapping_add(1);
                with_terminal_output_lock(&mut stderr, |stderr, state| {
                    write_status_line(stderr, &frame, &message, &mut state.visible_status_line_len);
                    let _ = stderr.flush();
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                with_terminal_output_lock(&mut stderr, |stderr, state| {
                    clear_status_line(stderr, &mut state.visible_status_line_len);
                    let _ = stderr.flush();
                });
                return;
            }
        }
    }
}

fn write_status_line<W: Write>(
    stderr: &mut W,
    marker: &str,
    message: &str,
    visible_status_line_len: &mut usize,
) {
    let line = format!("[{marker}] {message}");
    let pad_len = (*visible_status_line_len).saturating_sub(line.len());
    let _ = write!(stderr, "\r{line}{}", " ".repeat(pad_len));
    *visible_status_line_len = line.len();
}

fn clear_status_line<W: Write>(stderr: &mut W, visible_status_line_len: &mut usize) {
    if *visible_status_line_len > 0 {
        let _ = write!(stderr, "\r{}\r", " ".repeat(*visible_status_line_len));
        *visible_status_line_len = 0;
        let _ = stderr.flush();
    }
}

#[cfg(test)]
fn reset_terminal_output_state() {
    let coordinator = terminal_output_coordinator();
    let mut state = coordinator.state.lock().unwrap();
    *state = TerminalOutputState::default();
    coordinator.resume_condvar.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn test_mutex() -> &'static Mutex<()> {
        static TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        TEST_MUTEX.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn suspend_progress_output_clears_visible_status_line() {
        let _test_guard = test_mutex().lock().unwrap_or_else(|poison| poison.into_inner());
        reset_terminal_output_state();

        {
            let coordinator = terminal_output_coordinator();
            let mut state = coordinator.state.lock().unwrap();
            state.visible_status_line_len = 6;
        }

        let mut output = Vec::new();
        let suspension = suspend_progress_output_with_writer(&mut output);

        assert_eq!(String::from_utf8(output).unwrap(), "\r      \r");

        {
            let coordinator = terminal_output_coordinator();
            let state = coordinator.state.lock().unwrap();
            assert_eq!(state.paused_count, 1);
            assert_eq!(state.visible_status_line_len, 0);
        }

        drop(suspension);

        {
            let coordinator = terminal_output_coordinator();
            let state = coordinator.state.lock().unwrap();
            assert_eq!(state.paused_count, 0);
        }

        reset_terminal_output_state();
    }

    #[test]
    fn suspend_progress_output_is_reentrant() {
        let _test_guard = test_mutex().lock().unwrap_or_else(|poison| poison.into_inner());
        reset_terminal_output_state();

        let mut first_output = Vec::new();
        let mut second_output = Vec::new();
        let first = suspend_progress_output_with_writer(&mut first_output);
        let second = suspend_progress_output_with_writer(&mut second_output);

        {
            let coordinator = terminal_output_coordinator();
            let state = coordinator.state.lock().unwrap();
            assert_eq!(state.paused_count, 2);
        }

        drop(second);

        {
            let coordinator = terminal_output_coordinator();
            let state = coordinator.state.lock().unwrap();
            assert_eq!(state.paused_count, 1);
        }

        drop(first);

        {
            let coordinator = terminal_output_coordinator();
            let state = coordinator.state.lock().unwrap();
            assert_eq!(state.paused_count, 0);
        }

        reset_terminal_output_state();
    }
}
