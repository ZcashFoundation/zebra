{
  crane,
  lib,
}: {
  crane = import ./crane.nix {inherit crane lib;};

  ## Accepts separate configurations for nix-darwin, Home Manager, and NixOS, returning the correct
  ## one for whichever configuration is being built (guessing based on the attributes defined by
  ## `options`).
  ##
  ## This is useful for writing modules that work across multiple types of configuration.
  multiConfig = options: {
    darwin ? {},
    home ? {},
    nixos ? {},
  }: {
    config =
      if options ? homebrew
      then darwin
      else if options ? home
      then home
      else nixos;
  };
}
