//! "Reveal in files": shelling out to the platform's file manager to show a
//! script's backing file, per-OS

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Command;

/// Which platform [`reveal_command_for`] is building a command for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    /// The platform this build actually targets.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// The program and arguments [`reveal_in_file_manager`] invokes to reveal
/// `path` on `platform`: `open -R` on macOS (selects the file itself),
/// `explorer /select,` on Windows (same), and `xdg-open` on the file's
/// parent directory on Linux (no portable "select this file" verb exists
/// across desktop environments, so the containing folder opens instead).
#[must_use]
pub fn reveal_command_for(path: &Path, platform: Platform) -> (&'static str, Vec<OsString>) {
    match platform {
        Platform::MacOs => (
            "open",
            vec![OsString::from("-R"), path.as_os_str().to_owned()],
        ),
        Platform::Windows => {
            let mut arg = OsString::from("/select,");
            arg.push(path.as_os_str());
            ("explorer", vec![arg])
        }
        Platform::Linux => {
            // `Path::parent` returns `Some("")` (not `None`) for a bare
            // relative single-component path, so an empty parent must also
            // fall back to the path itself.
            let target = match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => path,
            };
            ("xdg-open", vec![target.as_os_str().to_owned()])
        }
    }
}

/// Shell out to the platform file manager to reveal `path`
///
/// # Errors
/// Returns the underlying [`io::Error`] if the platform command could not be
/// spawned (e.g. `xdg-open` is not installed).
#[tracing::instrument(name = "reveal_in_file_manager", skip(path), fields(path = %path.display()))]
pub fn reveal_in_file_manager(path: &Path) -> io::Result<()> {
    let (program, args) = reveal_command_for(path, Platform::current());
    let mut child = Command::new(program).args(&args).spawn()?;
    // Reap the child on a background thread rather than leaving it a
    // zombie: this process never needs its exit status, only that it not
    // accumulate one zombie per "Reveal in files" click.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    tracing::info!(path = %path.display(), "revealed in file manager");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::{Platform, reveal_command_for};

    #[test]
    fn macos_selects_the_file_itself_via_open_dash_r() {
        let (program, args) =
            reveal_command_for(Path::new("/home/t/work/migrate.sql"), Platform::MacOs);
        assert_eq!(program, "open");
        assert_eq!(
            args,
            vec![
                OsString::from("-R"),
                OsString::from("/home/t/work/migrate.sql")
            ]
        );
    }

    #[test]
    fn windows_selects_the_file_itself_via_explorer_select() {
        let (program, args) =
            reveal_command_for(Path::new("C:\\work\\migrate.sql"), Platform::Windows);
        assert_eq!(program, "explorer");
        assert_eq!(args, vec![OsString::from("/select,C:\\work\\migrate.sql")]);
    }

    #[test]
    fn linux_opens_the_files_containing_directory() {
        let (program, args) =
            reveal_command_for(Path::new("/home/t/work/migrate.sql"), Platform::Linux);
        assert_eq!(program, "xdg-open");
        assert_eq!(args, vec![OsString::from("/home/t/work")]);
    }

    #[test]
    fn linux_falls_back_to_the_path_itself_when_it_has_no_parent() {
        let (program, args) = reveal_command_for(Path::new("migrate.sql"), Platform::Linux);
        assert_eq!(program, "xdg-open");
        assert_eq!(args, vec![OsString::from("migrate.sql")]);
    }

    #[test]
    fn reveal_command_for_asserts_the_exact_path_for_a_session_script() {
        let path = Path::new("/data/sessions/1f3a/top-customers.sql");
        let (_, args) = reveal_command_for(path, Platform::Linux);
        assert_eq!(args, vec![OsString::from("/data/sessions/1f3a")]);
    }

    #[test]
    fn reveal_command_for_asserts_the_exact_path_for_a_library_script() {
        let path = Path::new("/data/library/revenue-report.sql");
        let (_, args) = reveal_command_for(path, Platform::MacOs);
        assert_eq!(
            args,
            vec![
                OsString::from("-R"),
                OsString::from("/data/library/revenue-report.sql")
            ]
        );
    }

    #[test]
    fn reveal_command_for_asserts_the_exact_path_for_an_external_file() {
        let path = Path::new("/home/t/reports/quarterly.sql");
        let (_, args) = reveal_command_for(path, Platform::MacOs);
        assert_eq!(
            args,
            vec![
                OsString::from("-R"),
                OsString::from("/home/t/reports/quarterly.sql")
            ]
        );
    }

    /// A non-UTF-8 path (valid on Linux/macOS) must reach the child process
    /// byte-for-byte, never mangled by `Path::display`'s lossy
    /// replacement-character conversion.
    #[test]
    #[cfg(unix)]
    fn a_non_utf8_path_is_never_corrupted_by_lossy_conversion() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/tmp/\xffinvalid-utf8.sql");
        let path = Path::new(raw);

        let (_, args) = reveal_command_for(path, Platform::Linux);

        assert_eq!(args, vec![OsString::from("/tmp")]);

        let (_, args) = reveal_command_for(path, Platform::MacOs);
        assert_eq!(args[1], OsString::from(raw));
    }
}
