#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::process::{Command, Output};

fn launch(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crusty"))
        .args(arguments)
        .creation_flags(0x0800_0000) // Exercise redirected output with no console.
        .output()
        .expect("launch crusty")
}

#[test]
fn executable_uses_gui_subsystem_including_debug_builds() {
    let executable = std::fs::read(env!("CARGO_BIN_EXE_crusty")).unwrap();
    let pe = u32::from_le_bytes(executable[0x3c..0x40].try_into().unwrap()) as usize;
    assert_eq!(&executable[pe..pe + 4], b"PE\0\0");
    let subsystem = pe + 24 + 68;
    assert_eq!(
        u16::from_le_bytes(executable[subsystem..subsystem + 2].try_into().unwrap()),
        2,
        "crusty must use IMAGE_SUBSYSTEM_WINDOWS_GUI, avoiding an empty console"
    );
}

#[test]
fn help_survives_output_redirection_without_a_console() {
    let output = launch(&["--help"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("Usage: crusty [PROJECT]"));
    assert!(help.contains("PIE_CRUST_MCP_TOKEN"));
}

#[test]
fn argument_errors_survive_redirection_and_return_failure() {
    for (arguments, expected) in [
        (vec!["--unknown"], "Unknown option"),
        (vec!["--mcp-port"], "requires a port"),
        (vec!["--mcp-port", "invalid"], "Invalid port"),
    ] {
        let output = launch(&arguments);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr).unwrap().contains(expected));
    }
}
