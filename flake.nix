{
  description = "Rust Overlay";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
      in with pkgs; {
        devShells.default = mkShell {
          buildInputs = [
            openssl
            pkg-config
            (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
          ];
          packages = [
            (pkgs.python3.withPackages
              (ps: with ps; [ pandas seaborn matplotlib numpy ]))
          ];
        };
      });
}
