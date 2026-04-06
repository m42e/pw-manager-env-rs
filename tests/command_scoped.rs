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
fn hook_outputs_powershell_wrappers_and_tracking() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_config_home = workspace.path().join("xdg");

    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(xdg_config_home.join("pw-env")).unwrap();
    std::fs::write(project_dir.join(".env"), "API_KEY=\n").unwrap();
    write_config(
        &xdg_config_home,
        &format!(
            "[[projects]]\npath = {:?}\ncommands = [\"cargo\", \"npm\"]\n",
            project_dir.to_string_lossy()
        ),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("hook")
        .arg(&project_dir)
        .arg("--shell")
        .arg("powershell")
        .env("XDG_CONFIG_HOME", &xdg_config_home)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("__pw_env_define_command_wrapper 'cargo'\n"));
    assert!(stdout.contains("__pw_env_define_command_wrapper 'npm'\n"));
    assert!(stdout.contains("$global:__pw_env_previous_keys = @()\n"));
    assert!(stdout.contains("$global:__pw_env_previous_commands = @('cargo', 'npm')\n"));
}

#[test]
#[cfg_attr(windows, ignore)]
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
#[cfg_attr(windows, ignore)]
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

fn set_executable(_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(_path, permissions).unwrap();
    }
}

#[test]
fn init_outputs_bash_hook() {
    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("init")
        .arg("bash")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("__pw_env_hook"));
}

#[test]
fn init_outputs_zsh_hook() {
    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("init")
        .arg("zsh")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("chpwd"));
}

#[test]
fn init_outputs_fish_hook() {
    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("init")
        .arg("fish")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--on-variable PWD"));
}

#[test]
fn config_template_prints_defaults() {
    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("config-template")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[defaults]"));
    assert!(stdout.contains("backend"));
}

#[test]
fn check_succeeds_even_without_backends() {
    let workspace = TempDir::new().unwrap();
    let xdg_config_home = workspace.path().join("xdg");
    std::fs::create_dir_all(xdg_config_home.join("pw-env")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("check")
        .env("XDG_CONFIG_HOME", &xdg_config_home)
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn approvals_list_shows_empty_when_no_approvals() {
    let workspace = TempDir::new().unwrap();
    let xdg_state_home = workspace.path().join("state");
    std::fs::create_dir_all(&xdg_state_home).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("list")
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No approved"));
}

#[test]
fn approvals_list_fetch_shows_empty_when_no_approvals() {
    let workspace = TempDir::new().unwrap();
    let xdg_state_home = workspace.path().join("state");
    std::fs::create_dir_all(&xdg_state_home).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("list-fetch")
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No approved"));
}

#[test]
fn approvals_approve_and_show_project_override() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_state_home = workspace.path().join("state");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&xdg_state_home).unwrap();
    std::fs::write(project_dir.join(".pw-env.toml"), "backend = \"op\"\n").unwrap();

    let approve = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("approve")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();
    assert!(approve.status.success(), "approve should succeed");

    let show = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("show")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();
    assert!(show.status.success(), "show should succeed");
    let stderr = String::from_utf8_lossy(&show.stderr);
    assert!(stderr.contains("approved") || stderr.contains("Status"));
}

#[test]
fn approvals_revoke_project_override() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_state_home = workspace.path().join("state");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&xdg_state_home).unwrap();
    std::fs::write(project_dir.join(".pw-env.toml"), "backend = \"op\"\n").unwrap();

    // Approve first
    Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("approve")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();

    // Then revoke
    let revoke = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("revoke")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();
    assert!(revoke.status.success(), "revoke should succeed");
    let stderr = String::from_utf8_lossy(&revoke.stderr);
    assert!(
        stderr.contains("Revoked") || stderr.contains("approval"),
        "unexpected output: {stderr}"
    );
}

#[test]
fn approvals_approve_fetch_and_show() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_state_home = workspace.path().join("state");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&xdg_state_home).unwrap();
    std::fs::write(project_dir.join(".env"), "API_KEY=\n").unwrap();

    let approve = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("approve-fetch")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();
    assert!(approve.status.success(), "approve-fetch should succeed");

    let show = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("show-fetch")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();
    assert!(show.status.success(), "show-fetch should succeed");
    let stderr = String::from_utf8_lossy(&show.stderr);
    assert!(
        stderr.contains("approved") || stderr.contains("Status"),
        "unexpected output: {stderr}"
    );
}

#[test]
fn approvals_revoke_fetch() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_state_home = workspace.path().join("state");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&xdg_state_home).unwrap();
    std::fs::write(project_dir.join(".env"), "API_KEY=\n").unwrap();

    // Approve first
    Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("approve-fetch")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();

    // Then revoke
    let revoke = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("approvals")
        .arg("revoke-fetch")
        .arg(&project_dir)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("HOME", workspace.path())
        .output()
        .unwrap();
    assert!(revoke.status.success(), "revoke-fetch should succeed");
}

#[test]
fn hook_outputs_empty_for_dir_without_env_file() {
    let workspace = TempDir::new().unwrap();
    let project_dir = workspace.path().join("project");
    let xdg_config_home = workspace.path().join("xdg");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(xdg_config_home.join("pw-env")).unwrap();
    // No .env file in project_dir

    let output = Command::new(env!("CARGO_BIN_EXE_pw-env"))
        .arg("hook")
        .arg(&project_dir)
        .env("XDG_CONFIG_HOME", &xdg_config_home)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.is_empty(), "expected empty output, got: {stdout}");
}