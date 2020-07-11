use std::env;
use std::path::PathBuf;

fn main() {
    // cc::Build::new().file("/home/jlusby/git/ecc/zcash/src/.libs/libzcashconsensus.a").cpp(true).compile("zcashconsensus");
    let host = "x86_64-unknown-linux-gnu";
    println!("cargo:rustc-link-lib=static=zcashconsensus");
    // println!("cargo:rustc-link-lib=static=bitcoin_util");
    println!("cargo:rustc-link-lib=static=boost_filesystem");
    println!("cargo:rustc-link-lib=static=boost_thread");
    println!("cargo:rustc-link-lib=static=boost_chrono");
    println!("cargo:rustc-link-lib=static=stdc++");
    println!("cargo:rustc-link-lib=static=sodium");
    println!("cargo:rustc-link-lib=static=secp256k1");
    println!("cargo:rustc-link-lib=static=crypto");
    println!("cargo:rustc-link-search=/home/jlusby/git/ecc/zcash/src/.libs");
    println!("cargo:rustc-link-search=/home/jlusby/git/ecc/zcash/src/");
    println!("cargo:rustc-link-search=/home/jlusby/git/ecc/zcash/src/zcash");
    println!("cargo:rustc-link-search=/home/jlusby/git/ecc/zcash/src/secp256k1/.libs");
    println!("cargo:rustc-link-search=/home/jlusby/git/ecc/zcash/depends/{}/lib/", host);
    println!("cargo:rustc-link-search=/usr/lib/gcc/x86_64-linux-gnu/9/");

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
