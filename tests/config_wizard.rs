use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::process::Command;
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
    let mut writer = pair.master.take_writer().unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let output_reader = Arc::clone(&output);
    let reader_thread = thread::spawn(move || {
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer).unwrap();
        output_reader.lock().unwrap().extend(buffer);
    });

    thread::sleep(Duration::from_millis(100));
    writer.write_all(input.as_bytes()).unwrap();
    writer.flush().unwrap();
    drop(writer);

    let deadline = Instant::now() + Duration::from_secs(1);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = reader_thread.join();
            let output = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
            panic!("config-wizard did not exit after PTY input; output: {output:?}");
        }
        thread::sleep(Duration::from_millis(10));
    };

    reader_thread.join().unwrap();
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
