use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn hook_outputs_exact_command_wrapper_without_path_match() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_config_home = workspace.path().join("xdg");

    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(xdg_config_home.join("pw-env")).unwrap();
    std::fs::write(project_dir.join(".env"), "API_KEY=\n").unwrap();
    write_config(
        &xdg_config_home,
        &format!(
            "[[projects]]\npath = {:?}\ncommands = [\"cargo\"]\n",
            project_dir.to_string_lossy()
        ),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("hook")
        .arg(&project_dir)
        .arg("--shell")
        .arg("bash")
        .env("XDG_CONFIG_HOME", &xdg_config_home)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("__pw_env_define_command_wrapper cargo\n"));
}

#[test]
fn hook_expands_globbed_command_wrappers_from_path() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_config_home = workspace.path().join("xdg");
    let bin_dir = workspace.path().join("bin");

    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(xdg_config_home.join("pw-env")).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(project_dir.join(".env"), "API_KEY=\n").unwrap();
    create_executable(&bin_dir.join("cargo"));
    create_executable(&bin_dir.join("cargo-watch"));
    create_executable(&bin_dir.join("npm"));

    write_config(
        &xdg_config_home,
        &format!(
            "[[projects]]\npath = {:?}\ncommands = [\"cargo*\"]\n",
            project_dir.to_string_lossy()
        ),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("hook")
        .arg(&project_dir)
        .arg("--shell")
        .arg("bash")
        .env("XDG_CONFIG_HOME", &xdg_config_home)
        .env("PATH", &bin_dir)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("__pw_env_define_command_wrapper cargo\n"));
    assert!(stdout.contains("__pw_env_define_command_wrapper cargo-watch\n"));
    assert!(!stdout.contains("__pw_env_define_command_wrapper npm\n"));
}

#[test]
fn exec_removes_managed_keys_from_child_environment() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_state_home = workspace.path().join("state");
    let bin_dir = workspace.path().join("bin");

    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&xdg_state_home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(project_dir.join(".env"), "HELLO=\n").unwrap();
    create_failing_executable(&bin_dir.join("op"));

    let approval = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("approve-fetch")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .output()
        .unwrap();

    assert!(approval.status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("exec")
        .arg("--dir")
        .arg(&project_dir)
        .arg("--")
        .arg("/usr/bin/env")
        .env("HELLO", "parent-value")
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .env("XDG_STATE_HOME", &xdg_state_home)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("HELLO=parent-value"));
}

fn write_config(xdg_config_home: &Path, contents: &str) {
    std::fs::write(xdg_config_home.join("pw-env/config.toml"), contents).unwrap();
}

fn create_executable(path: &Path) {
    std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    set_executable(path);
}

fn create_failing_executable(path: &Path) {
    std::fs::write(path, "#!/bin/sh\nexit 1\n").unwrap();
    set_executable(path);
}

fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }
}