// phonon.dll (Steam Audio) is loaded late: the game doesn't look for DLLs
// in our plugin folder, so `sound3d` loads it by full path first, and if it
// is missing the plugin still starts (with the classic acoustics).
fn main() {
    if std::env::var("TARGET").is_ok_and(|t| t.contains("windows")) {
        println!("cargo:rustc-link-arg=/DELAYLOAD:phonon.dll");
        println!("cargo:rustc-link-lib=delayimp");
    }
}
