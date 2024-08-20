# Zebra & Nix

This exposes a `zebrad` package in various ways, but it’s recommended to lean on one of the higher-level modules to integrate Zebra with your system.

## modules

### Home Manager

Zebra is generally intended to be run as a user service, so Home Manager tends to make the most sense for configuring it.

Home Manager can manage keeping your zebrad instance up and running with either systemd (Linux) or launchd (MacOS).

You first need to make this repository available to Home Manager. Here is a minimal flake for this purpose:

```nix
{
  inputs = {
    home-manager.url = "github:nix-community/home-manager/release-24.05";
    nixpkgs.url = "github:NixOS/nixpkgs/release-24.05";
    zebra.url = "github:sellout/zebra/nix-flake";
  };

  outputs = {home-manager, nixpkgs, self, zebra}: {
    homeConfigurations."<user>@<host>" = import ./home-configuration.nix {
      inherit home-manager nixpkgs zebra;
      system = "x86_64-linux";
    };
  };
}
```

See [home-configuration.nix](./home-configuration.nix) for how to connect Zebra to your configuration and [our test configuration](./modules/home.nix) for an example of how to set up Zebra.

### NixOS

Similarly, this can be run as a NixOS user service.

``` nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/release-24.05";
    zebra.url = "github:sellout/zebra/nix-flake";
  };

  outputs = {home-manager, nixpkgs, self, zebra}: {
    nixosConfigurations."<host>" = import ./nixos-configuration.nix {
      inherit nixpkgs zebra;
      system = "x86_64-linux";
    };
  };
}
```

### nix-darwin

Or as a nix-darwin user service.

```nix
{
  inputs = {
    nix-darwin.url = "github:LnL7/nix-darwin";
    nixpkgs.url = "github:NixOS/nixpkgs/release-24.05";
    zebra.url = "github:sellout/zebra/nix-flake";
  };

  outputs = {nix-darwin, nixpkgs, self, zebra}: {
    darwinConfigurations."<host>" = import ./darwin-configuration.nix {
      inherit nix-darwin nixpkgs zebra;
      system = "x86_64-linux";
    };
  };
}
```

## overlay

If you just want to access the applications directly, you can get them via the default overlay:

```nix
let
  newPkgs = pkgs.appendOverlays [zebra.overlays.default];
in [
  newPkgs.zebra-scanner
  newPkgs.zebrad
]
```

## execution

They can also be run directly, without installing anything.

```bash
nix run github:sellout/zebra/nix-flake#zebrad -- start
```
