{
  services.zebra = {
    ## If this is false, Zebra will not run, no matter what else is set in this section.
    enable = true;
    ## This exactly matches the structure of zebrad.toml, just using Nix syntax instead of TOML.
    config = {
      network.network = "Testnet";
      state.cache_dir = "/var/cache/zebrad";
      tracing = {
        ## This automatically enables the flamegraph support in Zebra.
        flamegraph = "/var/lib/zebrad/flamegraph";
        use_color = true;
      };
    };
    ## Other features to enable in the Zebra package. There is no need to list things that are
    ## required by entries in `services.zebra.config`, as those get enabled as needed (although
    ## adding them can improve Nix cache hits, since different configs on different machines won’t
    ## require different builds).
    extraFeatures = ["elasticsearch"];
  };

  boot.loader.grub.devices = ["/dev/sda1"];
  fileSystems."/".device = "/dev/sda1";
  system.stateVersion = "24.05";
}
