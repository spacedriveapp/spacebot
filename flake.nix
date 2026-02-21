{
  description = "Spacebot - An AI agent for teams, communities, and multi-user environments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
    };
  };

  outputs = { self, nixpkgs, flake-utils, crane, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };
        craneLib = crane.mkLib pkgs;
        bun = pkgs.bun;

        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
            ./migrations
            ./prompts
            ./interface/package.json
            ./interface/package-lock.json
            ./interface/index.html
            ./interface/tsconfig.json
            ./interface/tsconfig.node.json
            ./interface/vite.config.ts
            ./interface/postcss.config.js
            ./interface/tailwind.config.ts
            ./interface/public
            ./interface/src
          ];
        };

        spacebotPackages = import ./nix {
          inherit pkgs craneLib src;
        };

        inherit (spacebotPackages) spacebot spacebot-full;
      in
      {
        packages = {
          default = spacebot;
          inherit spacebot spacebot-full;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ spacebot ];
          buildInputs = with pkgs; [
            rustc
            cargo
            rust-analyzer
            clippy
            bun
            protobuf
            cmake
            openssl
            pkg-config
          ];
        };

        checks = {
          inherit spacebot;
        };
      }
    ) // {
      overlays.default = final: prev: {
        inherit (self.packages.${final.system}) spacebot spacebot-full;
      };

      nixosModules.default = import ./nix/module.nix self;
    };
}
