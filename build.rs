//! Decides whether slider lookups use PEXT, and exposes it as `cfg(pext)`.

use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(pext)");

    let is_x86_64 = env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch == "x86_64");
    let has_bmi2 = env::var("CARGO_CFG_TARGET_FEATURE")
        .is_ok_and(|features| features.split(',').any(|feature| feature == "bmi2"));
    let uses_pext = env::var_os("CARGO_FEATURE_PEXT").is_some();

    if is_x86_64 && has_bmi2 && uses_pext {
        println!("cargo::rustc-cfg=pext");
    }
}
