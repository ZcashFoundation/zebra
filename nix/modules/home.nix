{config, ...}: {
  services.zebra = {
    ## If this is false, Zebra will not run, no matter what else is set in this section.
    enable = true;
    ## This exactly matches the structure of zebrad.toml, just using Nix syntax instead of TOML.
    config = {
      network.network = "Testnet";
      state.cache_dir = "${config.xdg.cacheHome}/zebrad";
      tracing = {
        ## This automatically enables the flamegraph Cargo feature in Zebra.
        flamegraph = "${config.xdg.stateHome}/zebrad/flamegraph";
        use_color = true;
      };
    };
    ## Other Cargo features to enable in the Zebra package. There is no need to list things that are
    ## required by entries in `services.zebra.config`, as those get enabled as needed (although
    ## adding them can improve Nix cache hits, since different configs on different machines won’t
    ## require different builds).
    extraFeatures = ["elasticsearch"];
  };

  ## These attributes are simply required by Home Manager.
  home = {
    homeDirectory = /tmp/example;
    stateVersion = "24.05";
    username = "example-user";
  };
}
