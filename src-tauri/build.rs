use std::{env, process::Command};

fn main() {
    link_clang_runtime();
    tauri_build::build()
}

/// whisper.cpp's Metal backend uses `@available(macOS 15.0, *)`. When built
/// for an older deployment target (release builds target macOS 12), clang
/// lowers that to `__isPlatformVersionAtLeast`, which lives in clang's
/// runtime library; rustc links with `-nodefaultlibs`, so add it explicitly.
fn link_clang_runtime() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let output = Command::new("clang").arg("--print-runtime-dir").output();
    match output {
        Ok(out) if out.status.success() => {
            let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=static=clang_rt.osx");
        }
        _ => println!("cargo:warning=could not locate clang runtime; release link may fail"),
    }
}
