use std::{env, path::Path, process::Command};

fn git(directory: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

fn source_identity() {
    const SOURCE_OVERRIDE: &str = "OMNIAGENT_SOURCE_COMMIT";
    println!("cargo:rerun-if-env-changed={SOURCE_OVERRIDE}");
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
    let directory = Path::new(&manifest);
    let branch = git(directory, &["symbolic-ref", "--quiet", "HEAD"]);
    for name in ["HEAD", "index", "packed-refs"]
        .into_iter()
        .chain(branch.as_deref())
    {
        if let Some(path) = git(
            directory,
            &["rev-parse", "--path-format=absolute", "--git-path", name],
        ) && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let valid_commit = |value: &String| {
        matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    let commit = env::var(SOURCE_OVERRIDE)
        .ok()
        .filter(valid_commit)
        .or_else(|| git(directory, &["rev-parse", "--verify", "HEAD"]).filter(valid_commit))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let modified = git(
        directory,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=normal",
        ],
    )
    .map(|status| if status.is_empty() { "false" } else { "true" })
    .unwrap_or("unknown");
    println!("cargo:rustc-env=OMNIAGENT_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=OMNIAGENT_BUILD_DIRTY={modified}");
    for name in ["PROFILE", "TARGET"] {
        let value = env::var(name).expect("Cargo build identity");
        println!("cargo:rustc-env=OMNIAGENT_BUILD_{name}={value}");
    }
}

fn main() {
    source_identity();
    if env::var("TARGET").is_ok_and(|target| target.ends_with("-windows-msvc")) {
        // The Rust test executable does not embed Tauri's application manifest.
        // Mock Tauri apps link TaskDialogIndirect from Common Controls v6.
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
        // The desktop executable already carries the manifest embedded by Tauri.
        println!("cargo:rustc-link-arg-bin=omniagent=/MANIFEST:NO");
    }
    tauri_build::build();
}
