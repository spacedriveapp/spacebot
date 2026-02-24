{
  description = "Spacebot - An AI agent for teams, communities, and multi-user environments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
    };
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    crane,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {
          inherit system;
        };

        inherit (pkgs) bun;

        craneLib = crane.mkLib pkgs;

        cargoSrc = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./build.rs
            ./src
            ./migrations
            ./prompts
          ];
        };

        runtimeAssetsSrc = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./migrations
            ./prompts
          ];
        };

        frontendSrc = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./interface/package.json
            ./interface/bun.lock
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
          inherit pkgs craneLib cargoSrc runtimeAssetsSrc frontendSrc;
        };

        inherit (spacebotPackages) frontend spacebot spacebot-full spacebot-tests;
      in {
        packages = {
          default = spacebot;
          inherit frontend spacebot spacebot-full;
        };

        devShells = {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              rustfmt
              rust-analyzer
              clippy
              bun
              nodejs
              protobuf
              cmake
              openssl
              pkg-config
              onnxruntime
              chromium
            ];

            ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";
            CHROME_PATH = "${pkgs.chromium}/bin/chromium";
            CHROME_FLAGS = "--no-sandbox --disable-dev-shm-usage --disable-gpu";
          };

          backend = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              rustfmt
              rust-analyzer
              clippy
              protobuf
              cmake
              openssl
              pkg-config
              onnxruntime
            ];

            ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";
          };
        };

        checks = {
          inherit spacebot spacebot-full spacebot-tests;
        };
      }
    )
    // {
      overlays.default = final: {
        inherit (self.packages.${final.system}) spacebot spacebot-full;
      };

      nixosModules.default = import ./nix/module.nix self;
    };
}
