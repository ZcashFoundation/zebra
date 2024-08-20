{
  nixpkgs,
  system,
  zebra,
}:
nixpkgs.lib.nixosSystem {
  inherit system;
  modules = [
    {nixpkgs.overlays = [zebra.overlays.default];} # ← Makes `zebra` package available
    zebra.nixosModules.default #                     ← Makes `services.zebra` options available
    ./modules/configuration.nix #                    ← This file contains your Zebra configuration
  ];
}
