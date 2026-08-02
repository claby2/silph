{
  description = "silph - lightweight server monitoring stack";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        silph = pkgs.callPackage ./nix/package.nix { };
        default = silph;
      });

      overlays.default = final: prev: {
        silph = final.callPackage ./nix/package.nix { };
      };

      nixosModules = rec {
        silph = import ./nix/module.nix self;
        default = silph;
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rust-analyzer
            clippy
            rustfmt
          ];
        };
      });

      checks = forAllSystems (pkgs: {
        build = self.packages.${pkgs.system}.silph;

        # Evaluates the NixOS module with both services enabled (including
        # the tokenFile machinery) and materializes the resulting units, so
        # `nix flake check` catches option/rendering regressions without
        # building a full system.
        module-eval =
          let
            eval = nixpkgs.lib.nixosSystem {
              system = pkgs.system;
              modules = [
                self.nixosModules.default
                {
                  system.stateVersion = "25.11";
                  services.silph.collector = {
                    enable = true;
                    settings = {
                      listen = "0.0.0.0:9100";
                      token._secret = "/run/secrets/silph-token";
                      disk.mounts = [
                        "/"
                        "/home"
                      ];
                    };
                  };
                  services.silph.server = {
                    enable = true;
                    settings = {
                      scrape_interval = "30s";
                      targets = [
                        {
                          name = "secret-host";
                          url = "http://10.0.0.2:9100";
                          token._secret = "/run/secrets/silph-token";
                        }
                        {
                          name = "plain-host";
                          url = "http://10.0.0.3:9100";
                          token = "dummy";
                        }
                      ];
                    };
                  };
                }
              ];
            };
          in
          pkgs.writeText "silph-module-eval" (
            builtins.concatStringsSep "\n" [
              eval.config.systemd.units."silph-collector.service".text
              eval.config.systemd.units."silph-server.service".text
            ]
          );
      });
    };
}
