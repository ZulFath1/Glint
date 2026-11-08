//! Build step: puts Glint's icon into glint.exe.
//!
//! `assets/glint.rc` is compiled with Microsoft's resource compiler
//! (`rc.exe`, part of the Windows SDK that the MSVC toolchain needs anyway)
//! and the result is handed to the linker. No extra crates.
//!
//! If `rc.exe` can't be found, the build still works: the exe just has no
//! icon of its own, and a warning says so. Set the `RC` environment variable
//! to an `rc.exe` path to choose one by hand.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/glint.rc");
    println!("cargo:rerun-if-changed=assets/glint.ico");
    println!("cargo:rerun-if-env-changed=RC");

    // Only Windows with the MSVC linker takes a .res file.
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows-msvc") {
        return;
    }
    let Some(rc) = find_rc(&target) else {
        println!("cargo:warning=rc.exe (Windows SDK) not found: glint.exe gets no icon");
        return;
    };
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("glint.res");
    let status = Command::new(&rc)
        .args(["/nologo", "/I", "assets", "/fo"])
        .arg(&out)
        .arg("assets/glint.rc")
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bins={}", out.display());
        }
        other => {
            println!("cargo:warning=rc.exe failed ({other:?}): glint.exe gets no icon");
        }
    }
}

/// `RC` if set, else the newest Windows 10/11 SDK's rc.exe for this
/// machine's architecture.
fn find_rc(target: &str) -> Option<PathBuf> {
    if let Some(rc) = std::env::var_os("RC") {
        return Some(PathBuf::from(rc));
    }
    let arch = if target.starts_with("aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let roots = [
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        Some(PathBuf::from(r"C:\Program Files (x86)")),
    ];
    for root in roots.into_iter().flatten() {
        let bin = root.join("Windows Kits").join("10").join("bin");
        if let Some(rc) = newest_rc(&bin, arch) {
            return Some(rc);
        }
    }
    None
}

/// In `bin\<version>\<arch>\rc.exe`, the highest version that has one.
fn newest_rc(bin: &Path, arch: &str) -> Option<PathBuf> {
    let mut versions: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(bin)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let version: Vec<u32> = name
                .split('.')
                .map(|p| p.parse().ok())
                .collect::<Option<_>>()?;
            let rc = entry.path().join(arch).join("rc.exe");
            rc.exists().then_some((version, rc))
        })
        .collect();
    versions.sort();
    versions.pop().map(|(_, rc)| rc)
}
