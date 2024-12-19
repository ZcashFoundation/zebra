{
  config,
  lib,
  options,
  pkgs,
  ...
}: let
  cfg = config.services.zebra;
in {
  imports = [./generic.nix];

  config = lib.mkIf cfg.enable (let
    toml = pkgs.formats.toml {};

    configFile = toml.generate "zebrad.toml" cfg.config;
  in {
    environment = {
      etc."zebrad/zebrad.toml".source = configFile;
      systemPackages = [cfg.package];
    };

    launchd.agents.zebrad.serviceConfig = import ./launchd.nix {
      inherit configFile lib;
      inherit (cfg) package;
    };
  });
}
