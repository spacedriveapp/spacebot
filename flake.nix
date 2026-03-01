{
  description = "Spacebot - An AI agent for teams, communities, and multi-user environments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
    };
    bun2nix = {
      url = "github:nix-community/bun2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    crane,
    bun2nix,
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
          bun2nix = bun2nix.packages.${system}.default;
        };

        inherit (spacebotPackages) frontend spacebot spacebot-full spacebot-tests;

        frontendLockIntegrityCheck = pkgs.runCommand "frontend-lock-integrity-check" {
          nativeBuildInputs = [bun];
        } ''
          cp ${frontendSrc}/interface/bun.lock bun.lock

          bun --eval '
            const lockText = await Bun.file("bun.lock").text()
            let lock
            try {
              lock = JSON.parse(lockText.replace(/,\s*([}\]])/g, "$1"))
            } catch (error) {
              console.error("Failed to parse bun.lock for integrity check")
              console.error(error)
              process.exit(1)
            }
            const packageEntries = Object.values(lock.packages ?? {})
            const emptyIntegrities = packageEntries.filter(
              (entry) => Array.isArray(entry) && entry[3] === ""
            )

            if (emptyIntegrities.length > 0) {
              console.error("bun.lock has " + emptyIntegrities.length + " packages with empty integrity hashes")
              process.exit(1)
            }
          '

          touch $out
        '';
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
          inherit frontendLockIntegrityCheck spacebot spacebot-full spacebot-tests;
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
