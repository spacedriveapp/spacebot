{
  pkgs,
  craneLib,
  src,
}: let
  inherit (pkgs) lib onnxruntime;

  # Read version from Cargo.toml
  cargoToml = fromTOML (builtins.readFile "${src}/Cargo.toml");
  inherit (cargoToml.package) version;

  buildInputs = with pkgs; [
    protobuf
    cmake
    openssl
    pkg-config
    onnxruntime
  ];

  nativeBuildInputs = with pkgs; [
    pkg-config
    protobuf
    cmake
  ];

  frontend = pkgs.buildNpmPackage {
    pname = "spacebot-frontend";
    inherit version;
    src = "${src}/interface";

    npmDepsHash = "sha256-J11BrUDbZLKQDnS/+ux7QzK2vfLhtxUSZye0hIWnLPk=";
    npmInstallFlags = ["--legacy-peer-deps"];
    makeCacheWritable = true;

    installPhase = ''
      mkdir -p $out
      cp -r dist/* $out/
    '';
  };

  commonArgs = {
    inherit src nativeBuildInputs buildInputs;
    strictDeps = true;
    cargoExtraArgs = "";
  };

  cargoArtifacts = craneLib.buildDepsOnly (commonArgs
    // {
      preBuild = ''
        export ORT_LIB_LOCATION=${onnxruntime}/lib
      '';
    });

  spacebot = craneLib.buildPackage (commonArgs
    // {
      inherit cargoArtifacts;

      # Skip tests that require ONNX model file and known flaky suites in Nix builds
      cargoTestExtraArgs = "-- --skip memory::search::tests --skip memory::store::tests --skip config::tests::test_llm_provider_tables_parse_with_env_and_lowercase_keys";

      preBuild = ''
        export ORT_LIB_LOCATION=${onnxruntime}/lib
        export SPACEBOT_SKIP_FRONTEND_BUILD=1
        mkdir -p interface/dist
        cp -r ${frontend}/* interface/dist/
      '';

      postInstall = ''
        mkdir -p $out/share/spacebot
        cp -r ${src}/prompts $out/share/spacebot/
        cp -r ${src}/migrations $out/share/spacebot/
        chmod -R u+w $out/share/spacebot
      '';

      meta = with lib; {
        description = "An AI agent for teams, communities, and multi-user environments";
        homepage = "https://spacebot.sh";
        license = {
          shortName = "FSL-1.1-ALv2";
          fullName = "Functional Source License, Version 1.1, ALv2 Future License";
          url = "https://fsl.software/";
          free = true;
          redistributable = true;
        };
        platforms = platforms.linux ++ platforms.darwin;
        mainProgram = "spacebot";
      };
    });

  spacebot-full = pkgs.symlinkJoin {
    name = "spacebot-full";
    paths = [spacebot];

    buildInputs = [pkgs.makeWrapper];

    postBuild = ''
      wrapProgram $out/bin/spacebot \
        --set CHROME_PATH "${pkgs.chromium}/bin/chromium" \
        --set CHROME_FLAGS "--no-sandbox --disable-dev-shm-usage --disable-gpu" \
        --set ORT_LIB_LOCATION "${onnxruntime}/lib"
    '';

    meta =
      spacebot.meta
      // {
        description = spacebot.meta.description + " (with browser support)";
      };
  };
in {
  inherit spacebot spacebot-full;
}
