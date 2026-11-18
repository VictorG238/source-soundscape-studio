use std::{env, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=SSS_GIT_COMMIT");
    if let Some(directory) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo::rerun-if-changed={directory}/HEAD");
        println!("cargo::rerun-if-changed={directory}/refs");
        let packed = std::path::Path::new(&directory).join("packed-refs");
        if packed.exists() {
            println!("cargo::rerun-if-changed={}", packed.display());
        }
    }
    if let Some(commit) = env::var("SSS_GIT_COMMIT")
        .ok()
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .filter(|commit| {
            matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_hexdigit())
        })
    {
        println!("cargo::rustc-env=SSS_BUILD_COMMIT={commit}");
    }
    for name in ["PROFILE", "TARGET"] {
        if let Ok(value) = env::var(name) {
            println!("cargo::rustc-env=SSS_BUILD_{name}={value}");
        }
    }
}
