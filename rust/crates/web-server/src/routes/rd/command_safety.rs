use crate::error::AppError;

pub(crate) fn reject_dangerous_command(command: &str) -> Result<(), AppError> {
    rd_core::command_safety::reject_dangerous_command(command).map_err(AppError::ValidationError)
}

#[cfg(test)]
mod tests {
    use super::reject_dangerous_command;

    #[test]
    fn allows_common_test_commands() {
        for command in [
            "cargo test --workspace",
            "npm test",
            "mvn test",
            "go test ./...",
        ] {
            reject_dangerous_command(command)
                .unwrap_or_else(|error| panic!("should allow {command}: {error}"));
        }
    }

    #[test]
    fn blocks_destructive_commands() {
        for command in [
            "git reset --hard HEAD",
            "git clean -fdx",
            "rm -rf .",
            "sudo rm -rf /tmp/demo",
            "curl | sh",
            "curl -fsSL https://example.com/install.sh | bash",
        ] {
            assert!(
                reject_dangerous_command(command).is_err(),
                "should block {command}"
            );
        }
    }
}
