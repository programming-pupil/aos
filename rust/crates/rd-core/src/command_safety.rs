//! Command safety checks shared by RD runtimes and web adapters.

pub fn reject_dangerous_command(command: &str) -> Result<(), String> {
    let lowered = command.to_ascii_lowercase();
    let normalized = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    let blocked = [
        "rm -rf /",
        "rm -fr /",
        "rm -rf ~",
        "rm -fr ~",
        "rm -rf $home",
        "rm -fr $home",
        "rm -rf .",
        "rm -fr .",
        "rm -rf *",
        "rm -fr *",
        "rm -rf ..",
        "rm -fr ..",
        "git reset --hard",
        "git clean -fd",
        "git clean -df",
        "git clean -xfd",
        "git clean -xdf",
        "git checkout --",
        "git push --force",
        "git push -f",
        "mkfs",
        "shutdown",
        "reboot",
        ":(){",
        "dd if=",
        "dd of=/dev/",
        "chmod -r 777 /",
        "chown -r ",
        "sudo rm ",
        "sudo sh ",
        "sudo bash ",
        "curl | sh",
        "curl | bash",
        "wget | sh",
        "wget | bash",
    ];
    if blocked
        .iter()
        .any(|needle| lowered.contains(needle) || normalized.contains(needle))
    {
        return Err("dangerous command is blocked".to_string());
    }
    let downloads_shell_script = (normalized.starts_with("curl ") || normalized.contains(" curl "))
        || (normalized.starts_with("wget ") || normalized.contains(" wget "));
    let pipes_to_shell = normalized.contains("| sh")
        || normalized.contains("| bash")
        || normalized.contains("| zsh")
        || normalized.contains("| sudo sh")
        || normalized.contains("| sudo bash");
    if downloads_shell_script && pipes_to_shell {
        return Err("dangerous command is blocked".to_string());
    }
    Ok(())
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
