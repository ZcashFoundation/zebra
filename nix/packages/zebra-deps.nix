{
  clangStdenv,
  craneLib,
  lib,
  libclang,
  libiconv,
  src,
  stdenv,
  workspace,
}:
craneLib.buildDepsOnly (
  workspace
  // {
    inherit src;

    buildInputs = lib.optional stdenv.hostPlatform.isDarwin libiconv;

    stdenv = clangStdenv;

    LIBCLANG_PATH = libclang.lib + "/lib";
  }
)
