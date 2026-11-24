use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_rc_exe() -> Option<PathBuf> {
    if Command::new("rc.exe").arg("/?").output().is_ok() {
        return Some(PathBuf::from("rc.exe"));
    }
    let kits_bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if let Ok(entries) = std::fs::read_dir(kits_bin) {
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("x64").join("rc.exe"))
            .filter(|p| p.exists())
            .collect();
        versions.sort();
        if let Some(latest) = versions.pop() {
            return Some(latest);
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=assets/swipelist.ico");
    println!("cargo:rerun-if-changed=assets/swipelist.rc");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let rc_file = manifest_dir.join("assets").join("swipelist.rc");
    let res_file = out_dir.join("swipelist.res");

    if let Some(rc_exe) = find_rc_exe() {
        let status = Command::new(rc_exe)
            .arg("/nologo")
            .arg("/fo")
            .arg(&res_file)
            .arg(&rc_file)
            .status()
            .expect("failed to execute rc.exe");
        if status.success() {
            println!("cargo:rustc-link-arg-bins={}", res_file.display());
        } else {
            panic!("rc.exe failed to compile {}", rc_file.display());
        }
    } else {
        println!("cargo:warning=rc.exe not found; skipping Windows icon embedding");
    }
}
