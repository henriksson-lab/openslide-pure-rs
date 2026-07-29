use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let archive = out_dir.join("libnative_helpers.a");
    let mut objects = Vec::new();

    if env::var_os("CARGO_FEATURE_NATIVE_JPEG").is_some() {
        println!("cargo:rerun-if-changed=src/decode/jpeg_crop.c");
        println!("cargo:rustc-link-lib=jpeg");

        let jpeg_object = out_dir.join("jpeg_crop.o");
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
        objects.push(jpeg_object);
    }

    if env::var_os("CARGO_FEATURE_NATIVE_CAIRO_ORACLE").is_some() {
        println!("cargo:rerun-if-changed=src/decode/cairo_blit.c");

        let cairo_object = out_dir.join("cairo_blit.o");
        let cairo_cflags = pkg_config_args("--cflags", "cairo");
        let status = Command::new("cc")
            .args(["-std=c99", "-O2", "-fPIC"])
            .args(&cairo_cflags)
            .args(["-c", "src/decode/cairo_blit.c", "-o"])
            .arg(&cairo_object)
            .status()
            .expect("failed to run cc for cairo blit shim");
        assert!(status.success(), "cc failed while building cairo blit shim");
        objects.push(cairo_object);
    }

    if objects.is_empty() {
        return;
    }

    let mut ar = Command::new("ar");
    ar.arg("crus").arg(&archive);
    for object in &objects {
        ar.arg(object);
    }
    let status = ar
        .status()
        .expect("failed to run ar for native helper shims");
    assert!(
        status.success(),
        "ar failed while building native helper shims"
    );

    if env::var_os("CARGO_FEATURE_NATIVE_CAIRO_ORACLE").is_some() {
        for lib in pkg_config_args("--libs", "cairo") {
            if let Some(path) = lib.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={path}");
            } else if let Some(name) = lib.strip_prefix("-l") {
                println!("cargo:rustc-link-lib={name}");
            }
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
