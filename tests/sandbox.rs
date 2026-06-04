//! End-to-end tests for the Landlock filesystem sandbox.
//!
//! These drive the real compiled binary's hidden `__sandboxed-exec` helper
//! (which applies Landlock then execs `sh -c`), since the in-process unit tests
//! can't re-exec a real `enchanter`. Linux-only — Landlock is a Linux LSM.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

const SANDBOX_ARG: &str = "__sandboxed-exec";
const PATHS_ENV: &str = "ENCHANTER_SANDBOX_PATHS";

fn enchanter() -> &'static str {
    env!("CARGO_BIN_EXE_enchanter")
}

/// Run the sandbox helper with `allowed` as the single read/write root.
fn run_sandboxed(allowed: &Path, command: &str) -> std::process::Output {
    Command::new(enchanter())
        .arg(SANDBOX_ARG)
        .arg(command)
        .env(PATHS_ENV, allowed.to_string_lossy().to_string())
        .output()
        .expect("spawn enchanter sandbox helper")
}

/// Whether Landlock can actually be enforced on this kernel. If not, the helper
/// fails closed and we skip the assertions rather than report a false failure.
fn sandbox_available(allowed: &Path) -> bool {
    let out = run_sandboxed(allowed, "true");
    let stderr = String::from_utf8_lossy(&out.stderr);
    out.status.success() && !stderr.contains("Landlock")
}

#[test]
fn allows_read_inside_allowed_path() {
    let dir = std::env::temp_dir().join("enchanter_sb_allow");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("visible.txt");
    std::fs::write(&file, "VISIBLE_CONTENT").unwrap();

    if !sandbox_available(&dir) {
        eprintln!("skipping: Landlock not enforced on this kernel");
        return;
    }

    let out = run_sandboxed(&dir, &format!("cat {}", file.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("VISIBLE_CONTENT"), "stdout was: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn denies_read_outside_allowed_path() {
    // `allowed` is a fresh tmp dir. The "secret" lives under the project's
    // target/ dir (inside $HOME but outside `allowed`, /tmp, and system dirs),
    // so Landlock must block reading it.
    let allowed = std::env::temp_dir().join("enchanter_sb_deny_allowed");
    std::fs::create_dir_all(&allowed).unwrap();

    let secret_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&secret_dir).unwrap();
    let secret = secret_dir.join("enchanter_sb_secret.txt");
    std::fs::write(&secret, "TOPSECRET_CONTENT").unwrap();

    if !sandbox_available(&allowed) {
        eprintln!("skipping: Landlock not enforced on this kernel");
        let _ = std::fs::remove_file(&secret);
        return;
    }

    let out = run_sandboxed(&allowed, &format!("cat {}", secret.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("TOPSECRET_CONTENT"),
        "sandbox leaked file outside allowed path"
    );
    assert!(
        !out.status.success(),
        "expected non-zero exit when reading outside sandbox"
    );

    let _ = std::fs::remove_file(&secret);
    let _ = std::fs::remove_dir_all(&allowed);
}

#[test]
fn denies_write_outside_allowed_path() {
    let allowed = std::env::temp_dir().join("enchanter_sb_write_allowed");
    std::fs::create_dir_all(&allowed).unwrap();

    if !sandbox_available(&allowed) {
        eprintln!("skipping: Landlock not enforced on this kernel");
        return;
    }

    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("enchanter_sb_should_not_exist.txt");
    let _ = std::fs::remove_file(&target);

    let out = run_sandboxed(&allowed, &format!("echo pwned > {}", target.display()));

    assert!(
        !target.exists(),
        "sandbox allowed a write outside the allowed path"
    );
    assert!(
        !out.status.success(),
        "expected non-zero exit when writing outside sandbox"
    );

    let _ = std::fs::remove_dir_all(&allowed);
}
