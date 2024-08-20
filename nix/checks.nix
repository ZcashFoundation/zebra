{
  advisory-db,
  cargoArtifacts,
  craneLib,
  lib,
  pkgs,
  src,
  workspace,
}: {
  audit = craneLib.cargoAudit {inherit advisory-db src;};

  clippy = craneLib.cargoClippy (workspace
    // {
      inherit cargoArtifacts src;
      buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
        pkgs.darwin.apple_sdk.frameworks.Security
        pkgs.libiconv
      ];
      nativeBuildInputs = [pkgs.protobuf];
      stdenv = pkgs.clangStdenv;
      LIBCLANG_PATH = pkgs.libclang.lib + "/lib";
      cargoClippyExtraArgs = "--all-targets"; # -- --deny warnings";
    });

  deny = craneLib.cargoDeny (workspace
    // {
      inherit src;
      cargoDenyChecks = lib.concatStringsSep " " ["bans" "sources"];
    });

  doc = craneLib.cargoDoc (workspace
    // {
      inherit cargoArtifacts src;
      buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
        pkgs.darwin.apple_sdk.frameworks.Security
        pkgs.libiconv
      ];
      nativeBuildInputs = [pkgs.protobuf];
      stdenv = pkgs.clangStdenv;
      LIBCLANG_PATH = pkgs.libclang.lib + "/lib";
    });

  fmt = craneLib.cargoFmt (workspace // {inherit src;});

  # toml-fmt =
  #   craneLib.taploFmt (workspace // {src = lib.sources.sourceFilesBySuffices src [".toml"];});
}
