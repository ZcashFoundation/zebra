# usage:
#   nix-shell --pure zebrad.nix --run 'zebrad start'

let
  moz_overlay = import (builtins.fetchTarball https://github.com/mozilla/nixpkgs-mozilla/archive/master.tar.gz);
  nixpkgs = import <nixpkgs> { overlays = [ moz_overlay ]; };
in
  with nixpkgs;
  stdenv.mkDerivation {
    name = "zebra_shell";
    buildInputs = [
      cacert
      nixpkgs.latest.rustChannels.stable.rust
      # for a specific Rust version
      #(rustChannelOf { channel = "1.48"; }).rust
      clang
      llvmPackages.libclang
    ];

    LIBCLANG_PATH = llvmPackages.libclang.lib + "/lib";

    # There's possibly a cleaner way to do this using `carnix` or the standard
    # Nix Rust tooling. But I don't know if they support the Mozilla Rust
    # overlay, which we need for rustc 1.48.

    shellHook = ''
      # ignore the cargo config in $HOME
      export CARGO_HOME=$(mktemp -d)
      # put the targets in a temp dir
      export CARGO_TARGET_DIR=$(mktemp -d)

      # branch/tag/ref: use a specific git reference
      #   - required, because cargo defaults to the master branch
      # locked: use the exact versions in Cargo.lock (optional)
      # root/no-track: use a temporary install directory (optional)
      cargo install --git https://github.com/ZcashFoundation/zebra \
        --branch main \
        --locked \
        --root $(mktemp -d) \
        --no-track \
        zebrad
   '';
  }
