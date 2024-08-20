{
  fenix,
  pkgs,
}:
## TODO: This conditional is a workaround for nix-community/fenix#178. Use the `else`
##       unconditionally once that is fixed.
if pkgs.stdenv.isDarwin
then let
  fnx = fenix.packages.${pkgs.system};
in
  fnx.combine [
    (fnx.stable.cargo.overrideAttrs (old: {
      postBuild = ''
        install_name_tool \
          -change "/usr/lib/libcurl.4.dylib" "${pkgs.curl.out}/lib/libcurl.4.dylib" \
          ./cargo/bin/cargo
      '';
    }))
    fnx.stable.clippy
    fnx.stable.rustc
    fnx.stable.rustfmt
  ]
else
  fenix.packages.${pkgs.system}.fromToolchainFile {
    file = ../rust-toolchain.toml;
    sha256 = "s1RPtyvDGJaX/BisLT+ifVfuhDT1nZkZ1NcK8sbwELM=";
  }
