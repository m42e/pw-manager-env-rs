use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn run_config_wizard_in_pty(input: &str) -> (String, portable_pty::ExitStatus) {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_pw-env"));
    command.arg("config-wizard");

    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
    let output = Arc::new(Mutex::new(Vec::new()));
    let output_reader = Arc::clone(&output);
    let writer_for_reader = Arc::clone(&writer);
    let (reader_done_tx, reader_done_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        let mut pending = Vec::new();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let chunk = &buffer[..read];
                    output_reader.lock().unwrap().extend_from_slice(chunk);

                    pending.extend_from_slice(chunk);
                    while let Some(pos) = pending.windows(4).position(|window| window == b"\x1b[6n")
                    {
                        pending.drain(..pos + 4);
                        writer_for_reader
                            .lock()
                            .unwrap()
                            .write_all(b"\x1b[1;1R")
                            .unwrap();
                        writer_for_reader.lock().unwrap().flush().unwrap();
                    }

                    let keep_from = pending.len().saturating_sub(3);
                    pending.drain(..keep_from);
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = reader_done_tx.send(());
    });

    thread::sleep(Duration::from_millis(100));
    writer.lock().unwrap().write_all(input.as_bytes()).unwrap();
    writer.lock().unwrap().flush().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            drop(writer);
            child.kill().unwrap();
            drop(pair.master);
            let _ = reader_done_rx.recv_timeout(Duration::from_secs(1));
            let output = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
            panic!("config-wizard did not exit after PTY input; output: {output:?}");
        }
        thread::sleep(Duration::from_millis(10));
    };

    drop(writer);
    drop(pair.master);
    if reader_done_rx.recv_timeout(Duration::from_secs(1)).is_ok() {
        reader_thread.join().unwrap();
    }
    let output = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
    (output, status)
}

#[test]
fn config_wizard_requires_interactive_terminal() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("config-wizard")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "config-wizard should fail fast without an interactive terminal; stdout={:?} stderr={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }

    let output = child.wait_with_output().unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interactive terminal"),
        "stderr was: {stderr}"
    );
}

#[test]
fn config_wizard_runs_interactively_and_renders_initial_screen() {
    let (output, status) = run_config_wizard_in_pty("q\r");

    assert!(status.success(), "interactive run failed: {output}");
    assert!(output.contains("Questions"), "output was: {output:?}");
    assert!(output.contains("[defaults]"), "output was: {output:?}");
    assert!(
        output.contains("Config wizard cancelled."),
        "output was: {output:?}"
    );
}
