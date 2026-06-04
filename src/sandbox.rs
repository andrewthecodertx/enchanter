//! Filesystem sandbox for shell command execution.
//!
//! `exec_command` runs an arbitrary string through `sh -c`, which cannot be
//! confined from within Rust — the shell can `cd` anywhere, use absolute paths,
//! or pipe to other programs. The only robust boundary is the OS. On Linux we
//! use Landlock (a kernel LSM, unprivileged, inherited by children) to confine
//! the spawned shell to `allowed_paths` for read/write while still permitting
//! read+execute on the system directories needed to run programs.
//!
//! ## How it's applied
//!
//! Landlock restrictions are irrevocable and apply to the calling thread and
//! its descendants. Applying them in a post-`fork` `pre_exec` hook of our
//! multithreaded (tokio) process is unsafe (allocator-after-fork). Instead we
//! re-exec ourselves: the parent spawns `enchanter __sandboxed-exec <command>`
//! with the allowed paths passed via an environment variable. `main` intercepts
//! that argument *before* starting the async runtime, so the helper applies
//! Landlock on a single-threaded main thread and then `exec`s `sh -c <command>`.
//! The shell inherits the restrictions and the parent's piped stdio.

use std::path::PathBuf;

/// Hidden subcommand used for the re-exec sandbox helper.
pub const SANDBOX_ARG: &str = "__sandboxed-exec";

/// Environment variable carrying the newline-separated allowed paths to the
/// sandbox helper child.
pub const SANDBOX_PATHS_ENV: &str = "ENCHANTER_SANDBOX_PATHS";

/// Encode allowed paths for transport to the helper child via env var.
pub fn encode_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode_paths(raw: &str) -> Vec<PathBuf> {
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Entry point for the re-exec helper child (`enchanter __sandboxed-exec <cmd>`).
///
/// Reads the command from argv and allowed paths from the environment, applies
/// the filesystem sandbox to this (single-threaded) process, then replaces the
/// process image with `sh -c <command>`. Returns only on failure.
#[cfg(unix)]
pub fn run_sandboxed_child() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    let command = std::env::args()
        .nth(2)
        .ok_or_else(|| anyhow::anyhow!("{}: missing command argument", SANDBOX_ARG))?;
    let paths = std::env::var(SANDBOX_PATHS_ENV)
        .map(|raw| decode_paths(&raw))
        .unwrap_or_default();

    apply(&paths)?;

    // exec replaces this process; it only returns if the exec itself failed.
    let err = Command::new("sh").arg("-c").arg(&command).exec();
    Err(anyhow::Error::new(err).context("failed to exec sandboxed shell"))
}

#[cfg(not(unix))]
pub fn run_sandboxed_child() -> anyhow::Result<()> {
    anyhow::bail!("sandboxed exec is not supported on this platform")
}

// ── Linux: Landlock implementation ──

#[cfg(target_os = "linux")]
mod imp {
    use super::PathBuf;
    use landlock::{
        ABI, Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus,
    };
    use std::path::Path;

    /// System directories the sandboxed shell may read and execute from (so it
    /// can run programs and load shared libraries) but never write to.
    const SYSTEM_RX: &[&str] = &[
        "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/proc",
        "/sys", "/run",
    ];

    /// Scratch directories the shell may read and write in addition to
    /// `allowed_paths`.
    const SCRATCH_RW: &[&str] = &["/tmp", "/var/tmp"];

    /// Device files the shell may read and write (curated; never all of /dev).
    const DEV_RW: &[&str] = &[
        "/dev/null",
        "/dev/zero",
        "/dev/full",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
        "/dev/ptmx",
        "/dev/pts",
    ];

    /// Best guess at whether the running kernel can enforce Landlock. Used by the
    /// parent to decide whether to sandbox, fall back, or refuse.
    pub fn is_supported() -> bool {
        Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI::V1))
            .and_then(|r| r.create())
            .is_ok()
    }

    /// Apply Landlock filesystem confinement to the current thread/process.
    /// Read+write is granted under each path in `allowed_paths` (and scratch
    /// dirs); read+execute under system directories. Everything else is denied.
    pub fn apply(allowed_paths: &[PathBuf]) -> anyhow::Result<()> {
        let abi = ABI::V5;
        let access_rw = AccessFs::from_all(abi);
        let access_rx = AccessFs::from_read(abi);

        let mut ruleset = Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(access_rw)?
            .create()?;

        // Read+execute on system directories.
        for dir in SYSTEM_RX {
            if Path::new(dir).exists() {
                ruleset = ruleset.add_rule(PathBeneath::new(PathFd::new(dir)?, access_rx))?;
            }
        }

        // Read+write on the configured allowed paths plus scratch dirs.
        for path in allowed_paths
            .iter()
            .map(|p| p.to_path_buf())
            .chain(SCRATCH_RW.iter().map(PathBuf::from))
        {
            if path.exists() {
                ruleset = ruleset.add_rule(PathBeneath::new(PathFd::new(&path)?, access_rw))?;
            }
        }

        // Read+write on curated device files.
        for dev in DEV_RW {
            if Path::new(dev).exists() {
                ruleset = ruleset.add_rule(PathBeneath::new(PathFd::new(dev)?, access_rw))?;
            }
        }

        let status = ruleset.restrict_self()?;
        if status.ruleset == RulesetStatus::NotEnforced {
            anyhow::bail!("Landlock ruleset was not enforced by the kernel");
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub use imp::{apply, is_supported};

// ── Non-Linux stub ──

#[cfg(not(target_os = "linux"))]
pub fn is_supported() -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
pub fn apply(_allowed_paths: &[PathBuf]) -> anyhow::Result<()> {
    anyhow::bail!("filesystem sandbox is only available on Linux (Landlock)")
}
