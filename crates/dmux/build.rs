fn main() {
    // Short git sha for the sidebar version line.
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "dev".into());
    println!("cargo:rustc-env=DMUX_GIT_SHA={sha}");
    // Release builds get a tag from scripts/release.sh; empty = local dev
    // build, which disables the auto-updater (nothing to compare against).
    let tag = std::env::var("DMUX_BUILD_TAG").unwrap_or_default();
    println!("cargo:rustc-env=DMUX_BUILD_TAG={tag}");
    println!("cargo:rerun-if-env-changed=DMUX_BUILD_TAG");
}
