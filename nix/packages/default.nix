{
  callPackage,
  craneLib,
  src,
  workspace,
}: let
  zebra-deps = callPackage ./zebra-deps.nix {inherit craneLib src workspace;};
in {
  inherit zebra-deps;
  zebra = callPackage ./zebra {
    inherit craneLib src workspace;
    cargoArtifacts = zebra-deps;
  };
}
