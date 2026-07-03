use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/decode/jpeg_crop.c");
    println!("cargo:rerun-if-changed=src/decode/cairo_blit.c");
    println!("cargo:rerun-if-changed=src/decode/openjpeg_decode.c");
    println!("cargo:rustc-link-lib=jpeg");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let jpeg_object = out_dir.join("jpeg_crop.o");
    let cairo_object = out_dir.join("cairo_blit.o");
    let openjpeg_object = out_dir.join("openjpeg_decode.o");
    let archive = out_dir.join("libnative_helpers.a");

    let status = Command::new("cc")
        .args([
            "-std=c99",
            "-O2",
            "-fPIC",
            "-c",
            "src/decode/jpeg_crop.c",
            "-o",
        ])
        .arg(&jpeg_object)
        .status()
        .expect("failed to run cc for jpeg crop shim");
    assert!(status.success(), "cc failed while building jpeg crop shim");

    let cairo_cflags = pkg_config_args("--cflags", "cairo");
    let status = Command::new("cc")
        .args(["-std=c99", "-O2", "-fPIC"])
        .args(&cairo_cflags)
        .args(["-c", "src/decode/cairo_blit.c", "-o"])
        .arg(&cairo_object)
        .status()
        .expect("failed to run cc for cairo blit shim");
    assert!(status.success(), "cc failed while building cairo blit shim");

    let openjpeg_cflags = pkg_config_args("--cflags", "libopenjp2");
    let status = Command::new("cc")
        .args(["-std=c99", "-O2", "-fPIC"])
        .args(&openjpeg_cflags)
        .args(["-c", "src/decode/openjpeg_decode.c", "-o"])
        .arg(&openjpeg_object)
        .status()
        .expect("failed to run cc for OpenJPEG shim");
    assert!(status.success(), "cc failed while building OpenJPEG shim");

    let status = Command::new("ar")
        .arg("crus")
        .arg(&archive)
        .arg(&jpeg_object)
        .arg(&cairo_object)
        .arg(&openjpeg_object)
        .status()
        .expect("failed to run ar for native helper shims");
    assert!(
        status.success(),
        "ar failed while building native helper shims"
    );

    for lib in pkg_config_args("--libs", "cairo") {
        if let Some(path) = lib.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
        } else if let Some(name) = lib.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={name}");
        }
    }
    for lib in pkg_config_args("--libs", "libopenjp2") {
        if let Some(path) = lib.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
        } else if let Some(name) = lib.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={name}");
        }
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=native_helpers");
}

fn pkg_config_args(flag: &str, package: &str) -> Vec<String> {
    let output = Command::new("pkg-config")
        .args([flag, package])
        .output()
        .unwrap_or_else(|err| panic!("failed to run pkg-config {flag} {package}: {err}"));
    assert!(
        output.status.success(),
        "pkg-config {flag} {package} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("pkg-config output is UTF-8")
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}
