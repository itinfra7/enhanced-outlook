use std::process::Command;

fn owa_cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_owa-cli"))
}

#[test]
fn help_succeeds() {
    let out = owa_cli().arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("dump-eml"));
}

#[test]
fn unknown_command_fails() {
    let out = owa_cli().arg("no-such-command").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
