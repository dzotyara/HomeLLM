fn main() {
    // tauri-build embeds icons/icon.ico into the .exe but does not watch it:
    // without this a new icon only shows up after `cargo clean`.
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    tauri_build::build()
}
