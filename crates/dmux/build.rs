use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const MODULE_LINE_LIMIT: usize = 1_000;
const OVERSIZED_MODULES_FILE: &str = "oversized-modules.txt";

fn main() {
    enforce_module_line_limits();

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

fn enforce_module_line_limits() {
    println!("cargo:rerun-if-changed=../");
    println!("cargo:rerun-if-changed=../../{OVERSIZED_MODULES_FILE}");

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
    );
    let workspace_dir = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("dmux belongs to the workspace crates directory");
    let limits_path = workspace_dir.join(OVERSIZED_MODULES_FILE);
    let mut oversized_modules = parse_oversized_modules(&limits_path);
    let mut rust_files = Vec::new();
    collect_authored_rust_files(&workspace_dir.join("crates"), &mut rust_files);
    rust_files.sort();

    let mut violations = Vec::new();
    for path in rust_files {
        let relative = display_path(&path, workspace_dir);
        let relative_text = relative.to_string_lossy().replace('\\', "/");
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                violations.push(format!("could not inspect {relative_text}: {error}"));
                continue;
            }
        };
        let line_count = source.lines().count();
        match oversized_modules.remove(&relative_text) {
            Some(ceiling) if line_count == ceiling => {}
            Some(ceiling) if line_count > ceiling => violations.push(format!(
                "{relative_text} grew to {line_count} lines; its enforced ceiling is {ceiling}. Extract a cohesive module before adding lines"
            )),
            Some(ceiling) if line_count > MODULE_LINE_LIMIT => violations.push(format!(
                "{relative_text} shrank from {ceiling} to {line_count} lines. Lower its ceiling in {OVERSIZED_MODULES_FILE} to preserve the gain"
            )),
            Some(_) => violations.push(format!(
                "{relative_text} is now {line_count} lines. Remove it from {OVERSIZED_MODULES_FILE}"
            )),
            None if line_count > MODULE_LINE_LIMIT => violations.push(format!(
                "{relative_text} has {line_count} lines; the enforced limit is {MODULE_LINE_LIMIT}. Extract a cohesive module"
            )),
            None => {}
        }
    }

    for stale_path in oversized_modules.keys() {
        violations.push(format!(
            "{OVERSIZED_MODULES_FILE} contains {stale_path}, but that Rust file does not exist"
        ));
    }

    if !violations.is_empty() {
        for violation in &violations {
            println!("cargo:error={violation}");
        }
        panic!(
            "Rust module line-limit check failed with {} violation(s)",
            violations.len()
        );
    }
}

fn parse_oversized_modules(path: &Path) -> BTreeMap<String, usize> {
    let source = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "could not read the oversized Rust module ledger {}: {error}",
            path.display()
        )
    });
    let mut limits = BTreeMap::new();
    for (index, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (relative_path, ceiling) = line.split_once('=').unwrap_or_else(|| {
            panic!(
                "{}:{} must use path=line-count syntax",
                path.display(),
                index + 1
            )
        });
        let relative_path = relative_path.trim();
        let ceiling = ceiling.trim().parse::<usize>().unwrap_or_else(|error| {
            panic!(
                "{}:{} has an invalid line count: {error}",
                path.display(),
                index + 1
            )
        });
        assert!(
            ceiling > MODULE_LINE_LIMIT,
            "{}:{} only oversized modules belong in this ledger",
            path.display(),
            index + 1
        );
        assert!(
            limits.insert(relative_path.to_owned(), ceiling).is_none(),
            "{}:{} repeats {relative_path}",
            path.display(),
            index + 1
        );
    }
    limits
}

fn collect_authored_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_authored_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path
                .components()
                .any(|component| component.as_os_str() == "src" || component.as_os_str() == "tests")
        {
            files.push(path);
        }
    }
}

fn display_path<'a>(path: &'a Path, workspace_dir: &Path) -> &'a Path {
    path.strip_prefix(workspace_dir).unwrap_or(path)
}
