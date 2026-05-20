use std::env;

fn main() {
    if env::var_os("CARGO_FEATURE_POSTGRES_EXECUTION").is_none() {
        return;
    }

    let target = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target.as_str() {
        "macos" => println!("cargo:rustc-link-arg=-Wl,-export_dynamic"),
        "linux" | "freebsd" | "openbsd" | "netbsd" => {
            println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
        }
        _ => {}
    }
}
