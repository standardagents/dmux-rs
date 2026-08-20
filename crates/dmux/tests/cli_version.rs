use std::process::Command;

#[test]
fn version_exits_without_a_terminal_and_reports_build_metadata() {
    let output = Command::new(env!("CARGO_BIN_EXE_dmux-rs"))
        .arg("--version")
        .output()
        .expect("run dmux-rs --version");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let expected_version = if env!("DMUX_BUILD_TAG").is_empty() {
        format!("dev ({})", env!("DMUX_GIT_SHA"))
    } else {
        env!("DMUX_BUILD_TAG").to_owned()
    };
    assert_eq!(
        String::from_utf8(output.stdout).expect("version output is UTF-8"),
        format!("dmux-rs {expected_version}\n")
    );
}
