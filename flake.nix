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
        silph = pkgs.rustPlatform.buildRustPackage {
          pname = "silph";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          meta = {
            description = "Lightweight server monitoring stack";
            license = pkgs.lib.licenses.mit;
            mainProgram = "silph-server";
          };
        };
        default = silph;
      });

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
      });
    };
}
