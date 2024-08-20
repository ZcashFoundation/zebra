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
    home.packages = [cfg.package];

    launchd.agents.zebra = {
      enable = true;
      config = import ./launchd.nix {
        inherit configFile lib;
        inherit (cfg) package;
      };
    };

    systemd.user.services.zebrad = import ./systemd.nix {
      inherit configFile lib;
      inherit (cfg) package;
    };

    ## TODO: Maybe need to put this somewhere else on darwin.
    xdg.configFile."zebrad.toml".source = configFile;
  });
}
