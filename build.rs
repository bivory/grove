use std::process::Command;

fn main() {
    // Tell Cargo to rerun this if git HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/");
    println!("cargo:rerun-if-changed=.git/refs/tags/");

    // Get the git hash
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string());

    // Check if we're on a release tag (v*)
    let is_release = Command::new("git")
        .args(["describe", "--exact-match", "--tags", "HEAD"])
        .output()
        .is_ok_and(|output| output.status.success());

    // Check if working directory is dirty
    let is_dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty());

    let dirty_suffix = if is_dirty { "-dirty" } else { "" };

    // Set environment variables for use in the binary
    println!("cargo:rustc-env=GROVE_GIT_HASH={git_hash}{dirty_suffix}");
    println!("cargo:rustc-env=GROVE_IS_RELEASE={is_release}");
}
