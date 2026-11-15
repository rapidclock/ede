//! Route native syntax scanner allocations through the binary's worker arena.

fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    for name in ["malloc", "calloc", "realloc", "free"] {
        let argument = match target.as_str() {
            "macos" => format!("-Wl,-alias,_ede_{name},_{name}"),
            "linux" => format!("-Wl,--wrap={name}"),
            _ => continue,
        };
        println!("cargo:rustc-link-arg-bin=ede={argument}");
    }
}
