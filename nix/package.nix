{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "silph";
  version = "0.1.0";
  # Only what the build reads, so docs/nix-only changes don't rebuild it.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  meta = {
    description = "Lightweight server monitoring stack";
    license = lib.licenses.mit;
    mainProgram = "silph-server";
  };
}
