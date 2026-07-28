//! Test-only helper: a throwaway `ssh-agent` process for the agent-auth
//! integration test, torn down (process killed, `SSH_AUTH_SOCK` restored)
//! whenever it is dropped, including while a test is panicking.

use std::env;
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::NamedTempFile;

/// A running `ssh-agent` holding one identity, with this process's own
/// `SSH_AUTH_SOCK` pointed at it for the fixture's lifetime -- the only way
/// `zsql_ssh::SshAuth::Agent` discovers an agent, since it has no
/// explicit-socket-path variant.
pub struct ThrowawayAgent {
    pid: String,
    previous_sock: Option<String>,
}

impl ThrowawayAgent {
    /// Spawns a fresh `ssh-agent`, points this process's `SSH_AUTH_SOCK` at
    /// it, and `ssh-add`s `key_path` into it.
    ///
    /// Once the agent process itself has started, the rest of this method
    /// runs against an already-constructed `Self` so its `Drop` guard is
    /// armed even if a later step (parsing its output, `ssh-add`) panics --
    /// otherwise a panic before returning would leak the agent process,
    /// since nothing would exist yet to run `Drop` on.
    pub fn spawn(key_path: &Path) -> Self {
        let output = Command::new("ssh-agent")
            .arg("-s")
            .output()
            .expect("ssh-agent should be installed and runnable");
        assert!(
            output.status.success(),
            "ssh-agent exited with {:?}",
            output.status
        );
        let stdout = String::from_utf8(output.stdout).expect("ssh-agent output should be utf8");
        let pid = parse_sh_export(&stdout, "SSH_AGENT_PID")
            .expect("ssh-agent output should set SSH_AGENT_PID");

        let agent = Self {
            pid,
            previous_sock: env::var("SSH_AUTH_SOCK").ok(),
        };

        let sock = parse_sh_export(&stdout, "SSH_AUTH_SOCK")
            .expect("ssh-agent output should set SSH_AUTH_SOCK");
        set_ssh_auth_sock(Some(&sock));

        // `ssh-add` refuses a private key readable by anyone but its owner;
        // the repo checkout's own file mode is not that strict, so a
        // private, owner-only-permissioned copy is added instead of
        // `key_path` itself.
        let owner_only_key = owner_only_copy_of(key_path);
        let add_status = Command::new("ssh-add")
            .env("SSH_AUTH_SOCK", &sock)
            .arg(owner_only_key.path())
            .status()
            .expect("ssh-add should be runnable");
        assert!(
            add_status.success(),
            "ssh-add failed to load the fixture key"
        );

        agent
    }
}

/// Copies `path`'s contents into a fresh temp file with owner-only (0600)
/// permissions, satisfying `ssh-add`'s private-key permission check
/// regardless of how the source file itself is permissioned.
fn owner_only_copy_of(path: &Path) -> NamedTempFile {
    let contents = std::fs::read(path).expect("fixture key should be readable");
    let copy = NamedTempFile::new().expect("creating a temp file should succeed");
    std::fs::write(copy.path(), contents).expect("writing the key copy should succeed");
    std::fs::set_permissions(copy.path(), Permissions::from_mode(0o600))
        .expect("setting owner-only permissions should succeed");
    copy
}

impl Drop for ThrowawayAgent {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(&self.pid).status();
        set_ssh_auth_sock(self.previous_sock.as_deref());
    }
}

/// Sets (or, given `None`, unsets) this process's `SSH_AUTH_SOCK`.
///
/// Only this fixture reads or writes `SSH_AUTH_SOCK` in this test binary,
/// and the value is restored on drop, so the mutation stays scoped to this
/// fixture's own lifetime.
#[allow(unsafe_code)]
fn set_ssh_auth_sock(value: Option<&str>) {
    unsafe {
        match value {
            Some(value) => env::set_var("SSH_AUTH_SOCK", value),
            None => env::remove_var("SSH_AUTH_SOCK"),
        }
    }
}

/// Parses one `KEY=value; export KEY;` line out of `ssh-agent -s`'s
/// Bourne-shell-formatted stdout.
fn parse_sh_export(output: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    output.lines().find_map(|line| {
        let rest = line.strip_prefix(&prefix)?;
        let value = rest.split(';').next()?;
        Some(value.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::parse_sh_export;

    #[test]
    fn parse_sh_export_reads_the_named_variable() {
        let output = "SSH_AUTH_SOCK=/tmp/ssh-abc/agent.123; export SSH_AUTH_SOCK;\n\
                       SSH_AGENT_PID=456; export SSH_AGENT_PID;\n\
                       echo Agent pid 456;\n";
        assert_eq!(
            parse_sh_export(output, "SSH_AUTH_SOCK").as_deref(),
            Some("/tmp/ssh-abc/agent.123")
        );
        assert_eq!(
            parse_sh_export(output, "SSH_AGENT_PID").as_deref(),
            Some("456")
        );
    }

    #[test]
    fn parse_sh_export_returns_none_for_a_missing_variable() {
        let output = "SSH_AUTH_SOCK=/tmp/ssh-abc/agent.123; export SSH_AUTH_SOCK;\n";
        assert_eq!(parse_sh_export(output, "SSH_AGENT_PID"), None);
    }
}
