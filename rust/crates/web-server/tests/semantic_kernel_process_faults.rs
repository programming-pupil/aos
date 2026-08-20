use std::path::PathBuf;
use std::process::{Command, Output};

fn server_binary() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_web-server")
        .map(PathBuf::from)
        .expect("Cargo must provide the web-server binary for the process TCK")
}

fn run_phase(
    data_dir: &PathBuf,
    case: &str,
    mode: &str,
    fault_point: Option<&str>,
    trace_case: &str,
    key_ring: Option<&str>,
) -> Output {
    let mut command = Command::new(server_binary());
    command
        .arg("--semantic-kernel-process-tck")
        .arg(data_dir)
        .arg("--semantic-kernel-tck-case")
        .arg(case)
        .arg("--semantic-kernel-tck-mode")
        .arg(mode)
        .env("AOS_ALLOW_INSECURE_DEV_SECRETS", "1")
        .env("AOS_INTERNAL_PROCESS_TCK", "1")
        .env("JWT_SECRET", "semantic-kernel-process-tck-jwt-secret")
        .env("ENCRYPTION_KEY", "22222222222222222222222222222222")
        .env("ENCRYPTION_KEY_ID", "new-tck")
        .env(
            "TOKEN_ENCRYPTION_KEY",
            "semantic-kernel-process-tck-token-secret",
        )
        .env("AOS_BEHAVIOR_TRACE_CASE", trace_case);
    if let Some(point) = fault_point {
        command.env("AOS_PROCESS_FAULT_POINT", point);
    } else {
        command.env_remove("AOS_PROCESS_FAULT_POINT");
    }
    if let Some(key_ring) = key_ring {
        command.env("ENCRYPTION_KEY_RING", key_ring);
    } else {
        command.env_remove("ENCRYPTION_KEY_RING");
    }
    command
        .output()
        .expect("spawn semantic-kernel TCK server process")
}

fn temp_data_dir(case: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "aos-semantic-kernel-process-{case}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&path).expect("create process TCK data directory");
    path
}

fn assert_fault_exit(output: &Output, point: &str) {
    assert!(
        !output.status.success(),
        "faulted process unexpectedly succeeded: {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("AOS_PROCESS_FAULT\t{point}")),
        "fault marker missing for {point}: {stderr}"
    );
}

fn assert_recovered(output: &Output, case: &str) {
    assert!(
        output.status.success(),
        "recovery process failed for {case}:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("AOS_PROCESS_RESTART_EVIDENCE\t{case}")),
        "restart evidence missing for {case}: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn process_faults_survive_kill_restart() {
    let cases = [
        ("migration", "migration.after_commit"),
        ("turn", "turn_checkpoint.before_commit"),
        ("turn", "turn_checkpoint.after_commit"),
        ("interaction-question", "interaction.before_commit"),
        ("interaction-question", "interaction.after_commit"),
        ("interaction-approval", "interaction.before_commit"),
        ("interaction-approval", "interaction.after_commit"),
        ("interaction-credential", "interaction.before_commit"),
        ("interaction-credential", "interaction.after_commit"),
        ("interaction-oauth", "interaction.before_commit"),
        ("interaction-oauth", "interaction.after_commit"),
        ("tool", "tool_artifact.before_commit"),
        ("tool", "tool_artifact.after_commit"),
        ("compaction", "compaction.prepare.before_commit"),
        ("compaction", "compaction.prepare.after_commit"),
        ("compaction", "compaction.commit.before_commit"),
        ("compaction", "compaction.commit.after_commit"),
        ("memory", "memory.repository.before_return"),
        ("memory", "memory.repository.after_commit"),
        ("memory-consolidation", "memory.consolidation.before_commit"),
        ("memory-consolidation", "memory.consolidation.after_commit"),
    ];
    let mut trace = None;
    for (case, point) in cases {
        let data_dir = temp_data_dir(case);
        let prepare = run_phase(&data_dir, case, "prepare", Some(point), "FAULT-001", None);
        assert_fault_exit(&prepare, point);
        if trace.is_none() {
            trace = String::from_utf8_lossy(&prepare.stderr)
                .lines()
                .find(|line| line.contains("AOS_PRODUCTION_TRACE\tFAULT-001"))
                .map(ToOwned::to_owned);
        }
        let recover = run_phase(&data_dir, case, "recover", Some(point), "FAULT-001", None);
        assert_recovered(&recover, case);
        std::fs::remove_dir_all(data_dir).expect("remove process TCK data directory");
    }
    println!(
        "{}",
        trace.expect("production process TCK must emit its trace from the child server")
    );
}

#[test]
fn key_rotation_and_retirement_survive_process_restart() {
    let data_dir = temp_data_dir("rotation");
    let prepare = run_phase(
        &data_dir,
        "rotation",
        "prepare",
        Some("rotation.before_commit"),
        "KEY-001",
        Some(r#"{"old-tck":"11111111111111111111111111111111"}"#),
    );
    assert_fault_exit(&prepare, "rotation.before_commit");
    let trace = String::from_utf8_lossy(&prepare.stderr)
        .lines()
        .find(|line| line.contains("AOS_PRODUCTION_TRACE\tKEY-001"))
        .expect("production key TCK must emit its trace from the child server")
        .to_string();
    let recover = run_phase(
        &data_dir,
        "rotation",
        "recover",
        None,
        "KEY-001",
        Some(r#"{"old-tck":"11111111111111111111111111111111"}"#),
    );
    assert_recovered(&recover, "rotation");
    println!("{trace}");
    std::fs::remove_dir_all(data_dir).expect("remove process TCK data directory");

    let negative_dir = temp_data_dir("rotation-negative");
    let negative = run_phase(
        &negative_dir,
        "rotation-negative",
        "prepare",
        None,
        "KEY-NEG-001",
        Some(r#"{"old-tck":"11111111111111111111111111111111"}"#),
    );
    assert_recovered(&negative, "rotation-negative");
    std::fs::remove_dir_all(negative_dir).expect("remove negative rotation TCK data directory");
}
