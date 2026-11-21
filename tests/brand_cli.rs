use std::process::Command;

#[test]
fn public_command_reports_the_new_name() {
    let output = Command::new(env!("CARGO_BIN_EXE_betteragentview"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("betteragentview {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn legacy_command_keeps_its_machine_readable_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_agentview"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("agentview {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn public_help_uses_the_new_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_betteragentview"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Usage: betteragentview"));
}
