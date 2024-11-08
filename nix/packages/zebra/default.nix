{
  cargoArtifacts,
  clangStdenv,
  craneLib,
  darwin,
  lib,
  libclang,
  libiconv,
  protobuf,
  src,
  stdenv,
  workspace,
  ## Any additional features to compile zebrad with. See
  ## https://docs.rs/zebrad/latest/zebrad/index.html#zebra-feature-flags for available features.
  extraFeatures ? [],
  ## Whether to use exactly the dependency versions specified in the Cargo.lock file.
  locked ? true,
}:
craneLib.buildPackage (
  workspace
  // {
    inherit cargoArtifacts src;

    strictDeps = true;

    buildInputs =
      if stdenv.hostPlatform.isDarwin
      then [
        darwin.apple_sdk.frameworks.Security
        darwin.apple_sdk.frameworks.SystemConfiguration
        libiconv
      ]
      else [];

    nativeBuildInputs = [protobuf];

    stdenv = clangStdenv;

    LIBCLANG_PATH = libclang.lib + "/lib";

    cargoExtraArgs = lib.escapeShellArgs (
      ["--features" (lib.concatStringsSep "," extraFeatures)]
      ++ lib.optional locked "--locked"
    );

    ## TODO: Use the fixed-output derivation trick to allow network access during tests, so this can
    ##       be removed.
    ZEBRA_SKIP_NETWORK_TESTS = true;

    cargoTestExtraArgs =
      lib.escapeShellArgs
      (["--"]
        ++ lib.concatMap (test: ["--skip" test]) (import ./failing-tests.nix {inherit lib stdenv;}));

    meta.mainProgram = "zebrad";
  }
)
