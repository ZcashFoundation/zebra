{
  home-manager,
  nixpkgs,
  system,
  zebra,
}:
home-manager.lib.homeManagerConfiguration {
  modules = [
    {nixpkgs.overlays = [zebra.overlays.default];} # ← Makes `zebra` package available
    zebra.homeModules.default #                      ← Makes `services.zebra` options available
    ./modules/home.nix #                             ← This file contains your Zebra configuration
  ];
  pkgs = nixpkgs.legacyPackages.${system};
}
