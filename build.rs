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

    let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") else {
        println!("cargo:warning=CARGO_MANIFEST_DIR not set; skipping Windows icon embedding");
        return;
    };
    let Ok(out_dir) = env::var("OUT_DIR") else {
        println!("cargo:warning=OUT_DIR not set; skipping Windows icon embedding");
        return;
    };
    let rc_file = PathBuf::from(manifest_dir).join("assets").join("swipelist.rc");
    let res_file = PathBuf::from(out_dir).join("swipelist.res");

    if let Some(rc_exe) = find_rc_exe() {
        let status = match Command::new(rc_exe)
            .arg("/nologo")
            .arg("/fo")
            .arg(&res_file)
            .arg(&rc_file)
            .status()
        {
            Ok(status) => status,
            Err(err) => {
                println!(
                    "cargo:warning=failed to execute rc.exe ({err}); skipping Windows icon embedding"
                );
                return;
            }
        };
        if status.success() {
            println!("cargo:rustc-link-arg-bins={}", res_file.display());
        } else {
            println!(
                "cargo:warning=rc.exe failed to compile {} (status {status}); skipping Windows icon embedding",
                rc_file.display()
            );
        }
    } else {
        println!("cargo:warning=rc.exe not found; skipping Windows icon embedding");
    }
}
