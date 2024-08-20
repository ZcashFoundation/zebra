## TODO: This extracts various functions from the crane repo, bypassing `crane.mkLib`, since these
##       don’t require `pkgs`. (See ipetkov/crane#699)
{
  crane,
  lib,
}: let
  internalCrateNameFromCargoToml =
    import "${crane}/lib/internalCrateNameFromCargoToml.nix" {inherit lib;};
in {
  crateNameFromCargoToml =
    import "${crane}/lib/crateNameFromCargoToml.nix" {inherit internalCrateNameFromCargoToml lib;};

  filterCargoSources = import "${crane}/lib/filterCargoSources.nix" {inherit lib;};
}
