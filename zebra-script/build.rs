use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-link-lib=static=zcashconsensus");
    println!("cargo:rustc-link-search=/home/jlusby/git/ecc/zcash/src/.libs");

    println!("cargo:rerun-if-changed=cxx-src/zcashconsensus.h");

    let bindings = bindgen::Builder::default()
        .header("cxx-src/zcashconsensus.h")
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed.
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        // Finish the builder and generate the bindings.
        .generate()
        // Unwrap the Result and panic on failure.
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
