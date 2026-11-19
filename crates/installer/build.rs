//! Embeds the compilation target triple so the updater can pick the matching
//! release asset at runtime.

fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=OXID_RELAY_TARGET={target}");
}
