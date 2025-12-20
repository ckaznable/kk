#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() {
    println!("cargo:rustc-link-search=native=lib");
    println!("cargo:rustc-link-lib=mpv");

    let dll_name = "libmpv-2.dll";
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    
    let mut executable_path = out_dir.clone();
    executable_path.pop();
    executable_path.pop();
    executable_path.pop();

    let src = format!("./lib/{}", dll_name);
    let dest = executable_path.join(dll_name);

    if std::path::Path::new(&src).exists() {
        let _ = std::fs::copy(&src, dest);
    }
}
