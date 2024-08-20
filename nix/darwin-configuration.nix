{
  nix-darwin,
  nixpkgs,
  system,
  zebra,
}:
nix-darwin.lib.darwinSystem {
  pkgs = import nixpkgs {
    inherit system;
    overlays = [zebra.overlays.default]; # ← Makes `zebra` package available
  };
  modules = [
    zebra.darwinModules.default #          ← Makes `services.zebra` options available
    ./modules/darwin-configuration.nix #   ← This file contains your Zebra configuration
  ];
}
