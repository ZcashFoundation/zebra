{
  description = ''
    Zebra: the Zcash Foundation's independent, consensus-compatible implementation of a Zcash node
  '';

  nixConfig = {
    extra-experimental-features = ["no-url-literals"];
    extra-substituters = [
      "https://cache.garnix.io" # To benefit from the CI results
      "https://nix-community.cachix.org" # Contains rust toolchains
    ];
    extra-trusted-public-keys = [
      "cache.garnix.io:CTFPyKSLcx5RMJKfLo5EEPUObbA78b0YQ2DTCJXqr9g="
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
    use-registries = false;
    sandbox = "relaxed";
  };

  outputs = {
    advisory-db,
    crane,
    fenix,
    flake-utils,
    home-manager,
    nix-darwin,
    nixpkgs,
    self,
    systems,
  }: let
    ## Since they currently line up, we just use `nix-systems/default`, but see
    ## https://zebra.zfnd.org/user/supported-platforms.html for official support tiers.
    ##
    ## NixOS is not a supported OS, and this flake is not a supported distribution of Zebra, but
    ## here’s an approximation of what you can expect from this flake:
    ## - aarch64-darwin – tier 3
    ## - aarch64-linux (on Debian 11) – tier 3
    ## - x86_64-darwin – tier 2
    ## - x86_64-linux
    ##   - on Debian 11 – tier 1
    ##   - on Ubuntu “latest” – tier 2
    supportedSystems = import systems;

    lib = import ./nix/lib {
      inherit crane;
      inherit (nixpkgs) lib;
    };

    src = nixpkgs.lib.cleanSourceWith {
      src = ./.;
      filter = path: type:
        nixpkgs.lib.foldr
        (e: acc: nixpkgs.lib.hasSuffix e path || acc)
        true [".bin" ".proto" ".txt" ".utf8" ".vk"]
        || lib.crane.filterCargoSources path type;
    };

    ## This sets up some common attributes that _should_ be set in the workspace Cargo.toml, but
    ## aren’t.
    workspace =
      lib.crane.crateNameFromCargoToml {cargoToml = ./zebrad/Cargo.toml;} // {pname = "zebra";};

    mkCraneLib = pkgs:
      (crane.mkLib pkgs).overrideToolchain (import ./nix/rust-toolchain.nix {inherit fenix pkgs;});

    localPackages = {
      pkgs,
      craneLib ? mkCraneLib pkgs,
    }:
      import ./nix/packages {
        inherit craneLib src workspace;
        inherit (pkgs) callPackage;
      };
  in
    {
      overlays.default = final: prev: localPackages {pkgs = final;};

      darwinModules = {
        default = self.darwinModules.zebra;
        zebra = import ./nix/modules/zebra/darwin.nix;
      };
      homeModules = {
        default = self.homeModules.zebra;
        zebra = import ./nix/modules/zebra/home.nix;
      };
      nixosModules = {
        default = self.nixosModules.zebra;
        zebra = import ./nix/modules/zebra/nixos.nix;
      };

      ## For testing the `overlays` and  `darwinModules` as well as provide an example configuration
      ## for users.
      darwinConfigurations = builtins.listToAttrs (map (system: {
          name = "${system}-example";
          value = import ./nix/darwin-configuration.nix {
            inherit nix-darwin nixpkgs system;
            zebra = self;
          };
        })
        (builtins.filter (nixpkgs.lib.hasSuffix "-darwin") supportedSystems));

      ## For testing the `overlays` and  `homeModules` as well as provide an example configuration
      ## for users.
      homeConfigurations = builtins.listToAttrs (map (system: {
          name = "${system}-example";
          value = import ./nix/home-configuration.nix {
            inherit home-manager nixpkgs system;
            zebra = self;
          };
        })
        supportedSystems);

      ## For testing the `overlays` and  `nixosModules` as well as provide an example configuration
      ## for users.
      nixosConfigurations = builtins.listToAttrs (map (system: {
          name = "${system}-example";
          value = import ./nix/nixos-configuration.nix {
            inherit nixpkgs system;
            zebra = self;
          };
        })
        (builtins.filter (nixpkgs.lib.hasSuffix "-linux") supportedSystems));
    }
    // flake-utils.lib.eachSystem supportedSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};

      craneLib = mkCraneLib pkgs;

      packages = localPackages {inherit craneLib pkgs;};

      cargoArtifacts = packages.zebra-deps;
    in {
      apps =
        {default = self.apps.${system}.zebrad;}
        // (import ./nix/apps.nix {
          inherit flake-utils;
          drv = self.packages.${system}.zebra;
        });

      packages =
        {default = self.packages.${system}.zebra;}
        // packages;

      devShells.default = craneLib.devShell {
        inherit cargoArtifacts;
        checks = self.checks.${system};
        inputsFrom = builtins.attrValues self.packages.${system};
        packages = [pkgs.home-manager];
        LIBCLANG_PATH = pkgs.libclang.lib + "/lib";
      };

      checks = import ./nix/checks.nix {
        inherit advisory-db cargoArtifacts craneLib pkgs src workspace;
        inherit (nixpkgs) lib;
      };

      formatter = pkgs.alejandra;
    });

  inputs = {
    advisory-db = {
      flake = false;
      url = "github:rustsec/advisory-db";
    };

    crane.url = "github:ipetkov/crane";

    fenix = {
      inputs.nixpkgs.follows = "nixpkgs";
      url = "github:nix-community/fenix";
    };

    flake-utils = {
      inputs.systems.follows = "systems";
      url = "github:numtide/flake-utils";
    };

    home-manager = {
      inputs.nixpkgs.follows = "nixpkgs";
      url = "github:nix-community/home-manager/release-24.05";
    };

    nix-darwin = {
      inputs.nixpkgs.follows = "nixpkgs";
      url = "github:LnL7/nix-darwin";
    };

    nixpkgs.url = "github:NixOS/nixpkgs/release-24.05";

    systems.url = "github:nix-systems/default";
  };
}
