use std::{
    env,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::ffi::OsString;

use tokio::process::Command;

use super::guardian::{
    ProcessGuardianLaunch, encode_process_guardian_launch, process_guardian_command,
};

#[derive(Clone, Debug)]
pub(crate) struct LocalShell {
    executable: PathBuf,
}

impl LocalShell {
    pub(crate) async fn discover() -> io::Result<Self> {
        discover_shell().await.map(|executable| Self { executable })
    }

    pub(crate) fn guarded_command(
        &self,
        script: &str,
        cwd: &Path,
        foreground: bool,
    ) -> io::Result<(Command, Vec<u8>)> {
        if script.is_empty() || script.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal command is empty or contains NUL",
            ));
        }
        let environment = sanitized_environment()
            .into_iter()
            .map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| io::Error::other("terminal environment contains non-Unicode data"))?;
        let launch = ProcessGuardianLaunch::new(&self.executable, cwd, script, environment)
            .map_err(|_| io::Error::other("terminal guardian launch is invalid"))?;
        let frame = encode_process_guardian_launch(&launch)
            .map_err(|_| io::Error::other("terminal guardian frame is invalid"))?;
        let backend = guardian_backend_executable()?;
        let mut command = process_guardian_command(&backend, foreground)?;
        apply_sanitized_environment(&mut command);
        Ok((command, frame))
    }
}

pub(super) fn guardian_backend_executable() -> io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let Some(parent) = current.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "backend executable directory is unavailable",
        ));
    };
    if parent.file_name() != Some(OsStr::new("deps")) {
        return Ok(current);
    }

    let executable_name = if cfg!(target_os = "windows") {
        "synthchat-hermes-backend.exe"
    } else {
        "synthchat-hermes-backend"
    };
    let candidate = parent
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "target directory is unavailable"))?
        .join(executable_name);
    if !candidate.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "backend executable is unavailable for guardian tests",
        ));
    }
    std::fs::canonicalize(candidate)
}

pub(crate) async fn resolve_workspace_cwd(
    workspace_root: &Path,
    relative: Option<&Path>,
) -> io::Result<PathBuf> {
    let root_link_metadata = tokio::fs::symlink_metadata(workspace_root).await?;
    if root_link_metadata.file_type().is_symlink() || is_windows_reparse_point(&root_link_metadata)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "workspace root became a symbolic link or reparse point",
        ));
    }
    let canonical_root = tokio::fs::canonicalize(workspace_root).await?;
    let root_metadata = tokio::fs::metadata(&canonical_root).await?;
    if !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace root is not a directory",
        ));
    }

    let requested = relative
        .map(|relative| canonical_root.join(relative))
        .unwrap_or_else(|| canonical_root.clone());
    let canonical_requested = tokio::fs::canonicalize(requested).await?;
    if !canonical_requested.starts_with(&canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "terminal workdir escapes the registered workspace",
        ));
    }
    if !tokio::fs::metadata(&canonical_requested).await?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal workdir is not a directory",
        ));
    }
    Ok(canonical_requested)
}

#[cfg(target_os = "windows")]
fn is_windows_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(target_os = "windows"))]
fn is_windows_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn apply_sanitized_environment(command: &mut Command) {
    command.env_clear().envs(sanitized_environment());
}

pub(crate) fn sanitized_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut inherited = env::vars_os()
        .filter(|(name, _)| allowed_environment_name(name))
        .collect::<Vec<_>>();
    inherited.retain(|(name, _)| name != "TERM" && name != "NO_COLOR");
    inherited.push(("TERM".into(), "dumb".into()));
    inherited.push(("NO_COLOR".into(), "1".into()));
    inherited
}

fn allowed_environment_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };

    #[cfg(target_os = "windows")]
    let name = name.to_ascii_uppercase();
    #[cfg(target_os = "windows")]
    let name = name.as_str();

    matches!(
        name,
        "PATH"
            | "HOME"
            | "USER"
            | "USERNAME"
            | "LOGNAME"
            | "SHELL"
            | "TMP"
            | "TEMP"
            | "TMPDIR"
            | "LANG"
            | "LANGUAGE"
            | "LC_ALL"
            | "LC_CTYPE"
            | "LC_COLLATE"
            | "LC_MESSAGES"
            | "LC_MONETARY"
            | "LC_NUMERIC"
            | "LC_TIME"
            | "LC_PAPER"
            | "LC_NAME"
            | "LC_ADDRESS"
            | "LC_TELEPHONE"
            | "LC_MEASUREMENT"
            | "LC_IDENTIFICATION"
            | "TZ"
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
    ) || windows_runtime_environment_name(name)
}

#[cfg(target_os = "windows")]
fn windows_runtime_environment_name(name: &str) -> bool {
    matches!(
        name,
        "SYSTEMROOT"
            | "WINDIR"
            | "COMSPEC"
            | "PATHEXT"
            | "SYSTEMDRIVE"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
    )
}

#[cfg(not(target_os = "windows"))]
fn windows_runtime_environment_name(_name: &str) -> bool {
    false
}

#[cfg(target_os = "windows")]
async fn discover_shell() -> io::Result<PathBuf> {
    let mut candidates = Vec::new();
    for variable in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(root) = env::var_os(variable) {
            let root = PathBuf::from(root);
            if variable == "LOCALAPPDATA" {
                candidates.push(root.join("Programs/Git/bin/bash.exe"));
            } else {
                candidates.push(root.join("Git/bin/bash.exe"));
                candidates.push(root.join("Git/usr/bin/bash.exe"));
            }
        }
    }

    for candidate in candidates {
        if tokio::fs::metadata(&candidate)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return tokio::fs::canonicalize(candidate).await;
        }
    }
    for candidate in where_git_bash().await {
        if tokio::fs::metadata(&candidate)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return tokio::fs::canonicalize(candidate).await;
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Git Bash is required for terminal execution on Windows",
    ))
}

#[cfg(target_os = "windows")]
async fn where_git_bash() -> Vec<PathBuf> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt;

    let mut command = Command::new("where.exe");
    command.arg("git.exe");
    command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    let Ok(output) = command.output().await else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .flat_map(|line| {
            let git = PathBuf::from(line);
            let root = git.parent().and_then(Path::parent).map(Path::to_owned);
            root.into_iter()
                .flat_map(|root| [root.join("bin/bash.exe"), root.join("usr/bin/bash.exe")])
        })
        .collect()
}

#[cfg(target_os = "linux")]
async fn discover_shell() -> io::Result<PathBuf> {
    first_executable([PathBuf::from("/bin/bash"), PathBuf::from("/usr/bin/bash")]).await
}

#[cfg(target_os = "macos")]
async fn discover_shell() -> io::Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(shell) = env::var_os("SHELL") {
        let shell = PathBuf::from(shell);
        if shell.is_absolute() {
            candidates.push(shell);
        }
    }
    candidates.extend([PathBuf::from("/bin/zsh"), PathBuf::from("/bin/bash")]);
    first_executable(candidates).await
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn first_executable(candidates: impl IntoIterator<Item = PathBuf>) -> io::Result<PathBuf> {
    for candidate in candidates {
        if tokio::fs::metadata(&candidate)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return tokio::fs::canonicalize(candidate).await;
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "a compatible local shell is unavailable",
    ))
}

#[cfg(test)]
pub(crate) fn sanitized_environment_for_tests(
    values: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    values
        .into_iter()
        .filter(|(name, _)| allowed_environment_name(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_environment_uses_a_minimal_allowlist() {
        let values = [
            (OsString::from("PATH"), OsString::from("safe")),
            (OsString::from("HOME"), OsString::from("home")),
            (OsString::from("USER"), OsString::from("user")),
            (OsString::from("TMP"), OsString::from("tmp")),
            (OsString::from("LANG"), OsString::from("en_US.UTF-8")),
            (OsString::from("LC_TIME"), OsString::from("C")),
            (OsString::from("TZ"), OsString::from("UTC")),
            (OsString::from("openai_api_key"), OsString::from("secret")),
            (
                OsString::from("SYNTHCHAT_DESKTOP_TOKEN"),
                OsString::from("secret"),
            ),
            (OsString::from("DATABASE_URL"), OsString::from("secret")),
            (OsString::from("KUBECONFIG"), OsString::from("secret")),
            (
                OsString::from("DOCKER_AUTH_CONFIG"),
                OsString::from("secret"),
            ),
            (OsString::from("CI_JOB_JWT"), OsString::from("secret")),
            (OsString::from("NPM_CONFIG__AUTH"), OsString::from("secret")),
            (OsString::from("REDISCLI_AUTH"), OsString::from("secret")),
            (OsString::from("KRB5CCNAME"), OsString::from("secret")),
            (
                OsString::from("HTTPS_PROXY"),
                OsString::from("https://user:password@example.test"),
            ),
            (
                OsString::from("http_proxy"),
                OsString::from("http://user:password@example.test"),
            ),
        ];
        let sanitized = sanitized_environment_for_tests(values);
        assert_eq!(
            sanitized,
            vec![
                (OsString::from("PATH"), OsString::from("safe")),
                (OsString::from("HOME"), OsString::from("home")),
                (OsString::from("USER"), OsString::from("user")),
                (OsString::from("TMP"), OsString::from("tmp")),
                (OsString::from("LANG"), OsString::from("en_US.UTF-8")),
                (OsString::from("LC_TIME"), OsString::from("C")),
                (OsString::from("TZ"), OsString::from("UTC")),
            ]
        );
    }

    #[test]
    fn platform_runtime_environment_is_allowlisted_explicitly() {
        let values = [
            (OsString::from("SystemRoot"), OsString::from("C:\\Windows")),
            (OsString::from("ComSpec"), OsString::from("cmd.exe")),
            (OsString::from("PATHEXT"), OsString::from(".EXE;.CMD")),
        ];
        let sanitized = sanitized_environment_for_tests(values);
        #[cfg(target_os = "windows")]
        assert_eq!(sanitized.len(), 3);
        #[cfg(not(target_os = "windows"))]
        assert!(sanitized.is_empty());
    }
}
