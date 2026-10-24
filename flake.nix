{
  description = "An MCP bridge converted from CLI for better permission control";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      manifest = with builtins; fromTOML (readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          lib = pkgs.lib;
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = manifest.package.name;
            src = lib.cleanSource ./.;
            version = manifest.package.version;
            cargoLock.lockFile = ./Cargo.lock;

            meta = {
              licenses = lib.licenses.mit;
              mainProgram = manifest.package.name;
            };
          };
        }
      );
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          ciDeps = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
          ];
          devDeps = with pkgs; [
            rust-analyzer
            taplo
          ];
        in
        {
          default = pkgs.mkShell {
            buildInputs = ciDeps ++ devDeps;
          };
        }
      );
    };
}
