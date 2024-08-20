{
  drv,
  flake-utils,
}: let
  mkNamedApp = name: {
    inherit name;
    value = flake-utils.lib.mkApp {inherit drv name;};
  };
in
  ## There are additional apps in the workspace, but they are only produced when particular
  ## Cargo features are enabled.
  builtins.listToAttrs (map mkNamedApp [
    "zebra-scanner"
    "zebrad"
  ])
