{
  bash,
  cacert,
  cargo-nextest,
  direnv,
  fetchurl,
  git,
  gnugrep,
  hk,
  jujutsu,
  lefthook,
  lib,
  pkl,
  prek,
  pre-commit,
  procps,
  python3,
  rustPlatform,
  stdenv,
  util-linux,
  uv,
  ...
}:
let
  cargoToml = lib.importTOML ../Cargo.toml;
  pythonWithPreCommit = python3.withPackages (pythonPackages: [
    (pythonPackages.toPythonModule pre-commit)
  ]);
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../.cargo/config.toml
      ../Cargo.toml
      ../Cargo.lock
      ../README.md
      ../src
      ../tests
    ];
  };
  hkConfigPackage = fetchurl {
    name = "hk-pkl-1.45.0.zip";
    url = "https://github.com/jdx/hk/releases/download/v1.45.0/hk@1.45.0.zip";
    hash = "sha256-EkgZ0/9V7tmILzLCz0eKjzDdFNCXLRkMGsK1kT8OZ6M=";
  };
  hkConfigMetadata = fetchurl {
    name = "hk-pkl-1.45.0.json";
    url = "https://github.com/jdx/hk/releases/download/v1.45.0/hk@1.45.0";
    hash = "sha256-4oMh+GfjMw6x1hPlauatR1vpaYNzTd/tOqvHHbXVUsM=";
  };
in
rustPlatform.buildRustPackage {
  pname = "jj-hooks";
  version = cargoToml.package.version;
  inherit src;

  cargoLock.lockFile = ../Cargo.lock;

  doCheck = true;
  useNextest = true;
  cargoTestFlags = [ "--no-fail-fast" ];
  # Checks select standalone Pkl and its seeded offline cache; pklr has no disk cache.
  # This check-only setting leaves hk's installed runtime default unchanged.
  env.HK_PKL_BACKEND = "pkl";
  env.SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";
  nativeCheckInputs = [
    bash
    cacert
    cargo-nextest
    direnv
    git
    gnugrep
    hk
    jujutsu
    lefthook
    pkl
    prek
    pythonWithPreCommit
    uv
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [
    procps
    util-linux
  ];

  # hk fixtures import version 1.45.0. Seed Pkl's normal package cache with
  # the exact zip and metadata served by the upstream release.
  preCheck = ''
    export HOME="$TMPDIR/jj-hooks-test-home"
    export JAVA_TOOL_OPTIONS="-Duser.home=$HOME"
    export XDG_CACHE_HOME="$HOME/.cache"
    export XDG_CONFIG_HOME="$HOME/.config"
    export XDG_STATE_HOME="$HOME/.local/state"
    mkdir -p "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"

    pklCache="$HOME/.pkl/cache/package-2/github.com/jdx/hk/releases/download/v1.45.0/hk@1.45.0"
    mkdir -p "$pklCache"
    cp ${hkConfigPackage} "$pklCache/hk@1.45.0.zip"
    cp ${hkConfigMetadata} "$pklCache/hk@1.45.0.json"
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    for binary in jj-hooks jj-hp; do
      "$out/bin/$binary" --help >/dev/null
      "$out/bin/$binary" --version >/dev/null
    done
    runHook postInstallCheck
  '';

  meta = {
    description = "Run hook frameworks before pushing jj bookmarks";
    homepage = "https://github.com/mattwilkinsonn/jj-hooks";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "jj-hp";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
      "x86_64-darwin"
      "aarch64-darwin"
    ];
  };
}
