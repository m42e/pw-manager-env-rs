use std::process::Command;

#[test]
fn config_wizard_requires_interactive_terminal() {
    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("config-wizard")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interactive terminal"),
        "stderr was: {stderr}"
    );
}
