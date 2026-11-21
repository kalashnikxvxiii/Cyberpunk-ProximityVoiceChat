// Where phonon (Steam Audio) is: third_party/steamaudio, from
// tools/fetch-steamaudio.sh. On Windows the plugin loads phonon.dll late and
// by full path (see the plugin's build.rs); on Linux (the tests) the library
// is found through an rpath.
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../third_party/steamaudio/lib");
    let target = std::env::var("TARGET").unwrap();
    let dir = if target.contains("windows") { root.join("windows-x64") } else { root.join("linux-x64") };
    if !dir.exists() {
        panic!("Steam Audio not found in {}: run tools/fetch-steamaudio.sh", dir.display());
    }
    let dir = dir.canonicalize().unwrap();
    println!("cargo:rustc-link-search=native={}", dir.display());
    if !target.contains("windows") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
    }
    println!("cargo:rerun-if-changed=../../third_party/steamaudio/.version");
}
