use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native/menu-bar.m");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => panic!("unsupported macOS architecture"),
    };
    let object = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("menu-bar.o");
    let status = Command::new("clang")
        .args(["-arch", arch, "-fobjc-arc", "-c", "native/menu-bar.m", "-o"])
        .arg(&object)
        .status()
        .expect("run clang to compile the macOS menu bar");
    assert!(status.success(), "could not compile the macOS menu bar");
    println!(
        "cargo:rustc-link-arg-bin=zotero-cloud-bridge={}",
        object.display()
    );
    println!("cargo:rustc-link-lib=framework=AppKit");
}
