use assert_cmd::Command;

#[test]
fn test_stdin_success() {
    let mut cmd = Command::cargo_bin("typos").unwrap();
    cmd.arg("-").write_stdin("Hello world");
    cmd.assert().success();
}

#[test]
fn test_stdin_failure() {
    let mut cmd = Command::cargo_bin("typos").unwrap();
    cmd.arg("-").write_stdin("Apropriate world");
    cmd.assert().code(2);
}

#[test]
fn test_stdin_correct() {
    let mut cmd = Command::cargo_bin("typos").unwrap();
    cmd.arg("-")
        .arg("--write-changes")
        .write_stdin("Apropriate world");
    cmd.assert().success().stdout("Appropriate world");
}
