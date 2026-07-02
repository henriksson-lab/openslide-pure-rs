use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/decode/jpeg_crop.c");
    println!("cargo:rustc-link-lib=jpeg");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let object = out_dir.join("jpeg_crop.o");
    let archive = out_dir.join("libjpeg_crop.a");

    let status = Command::new("cc")
        .args([
            "-std=c99",
            "-O2",
            "-fPIC",
            "-c",
            "src/decode/jpeg_crop.c",
            "-o",
        ])
        .arg(&object)
        .status()
        .expect("failed to run cc for jpeg crop shim");
    assert!(status.success(), "cc failed while building jpeg crop shim");

    let status = Command::new("ar")
        .arg("crus")
        .arg(&archive)
        .arg(&object)
        .status()
        .expect("failed to run ar for jpeg crop shim");
    assert!(status.success(), "ar failed while building jpeg crop shim");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=jpeg_crop");
}
