use std::process::Command;

use tempfile::tempdir;

const GUARDRAILS_ACKNOWLEDGEMENT_FLAG: &str =
    "--i-understand-that-this-will-be-running-without-the-usual-guardrails";

#[test]
// SPEC 17.7: CLI surfaces startup failure for a missing explicit workflow path.
fn missing_workflow_file_returns_error_exit() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let missing_path = directory.path().join("missing-workflow.md");

    let output = Command::new(env!("CARGO_BIN_EXE_symphony"))
        .arg(GUARDRAILS_ACKNOWLEDGEMENT_FLAG)
        .arg(&missing_path)
        .output()?;

    assert_eq!(output.status.code(), Some(1));

    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("missing_workflow_file"));

    Ok(())
}
