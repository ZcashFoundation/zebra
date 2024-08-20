{
  config,
  lib,
  options,
  pkgs,
  ...
}: let
  cfg = config.services.zebra;
in {
  options.services.zebra = {
    enable = lib.mkEnableOption "Zebra";

    basePackage = lib.mkPackageOption pkgs "Zebra" {default = ["zebra"];};

    package = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      description = ''
        The _actual_ package, built from the base package with `extraFeatures` applied.
      '';
    };

    config = lib.mkOption {
      type = lib.types.attrs;
      default = {
        tracing.use_journald = pkgs.stdenv.hostPlatform.isLinux;
      };
      defaultText = lib.literalExpression ''
        {
          tracing.use_journald = pkgs.stdenv.hostPlatform.isLinux;
        }
      '';
      description = "A Nix attribute set of the values to put in zebrad.toml";
      example = {
        tracing.progress_bar = "summary";
      };
    };

    extraFeatures = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = ''
        A list of optional features to enable in the package. See
        https://docs.rs/zebrad/latest/zebrad/index.html#zebra-feature-flags
        for the avaliable values.

        __NB__: Some features are enabled automatically if config options that require them are set.
                For example, `services.zebra.config.tracing.use_journald = true` will cause
               `"journald"` to be enabled and
               `services.zebra.config.tracing.flamegraph = "some/path"` will cause `"flamegraph"` to
                be enabled.
      '';
      example = ["elasticsearch" "journald" "prometheus"];
    };
  };

  config = lib.mkIf cfg.enable (let
    combinedFeatures =
      cfg.extraFeatures
      ++ lib.optional (cfg.config.tracing.flamegraph or null != null) "flamegraph"
      ++ lib.optional (cfg.config.tracing.use_journald or false) "journald";
  in {
    services.zebra.package = cfg.basePackage.override {extraFeatures = combinedFeatures;};
  });
}
