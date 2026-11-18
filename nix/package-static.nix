# Release build of jj-hooks and jj-hp for nebula CD (`kind: "nix"` in
# component.cue). A release asset must run without this build's /nix/store, so
# the binaries are portable: on Linux, pkgsStatic makes them static musl ELFs;
# on darwin, which cannot link fully static, they link only the system
# libSystem under /usr/lib. CD's check-asset gate enforces both rules.
#
# The suite runs in package.nix (checks.package); this build only smoke-tests.
{
  lib,
  pkgs,
}:
let
  cargoToml = lib.importTOML ../Cargo.toml;
  target = if pkgs.stdenv.hostPlatform.isLinux then pkgs.pkgsStatic else pkgs;
in
target.rustPlatform.buildRustPackage {
  pname = "jj-hooks-static";
  version = cargoToml.package.version;
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

  cargoLock.lockFile = ../Cargo.lock;

  doCheck = false;

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    for binary in jj-hooks jj-hp; do
      "$out/bin/$binary" --version >/dev/null
    done
  ''
  + lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
    # Fail here, not at check-asset, if a dylib comes from the store.
    for binary in jj-hooks jj-hp; do
      if otool -L "$out/bin/$binary" | grep -q /nix/store; then
        otool -L "$out/bin/$binary"
        exit 1
      fi
    done
  ''
  + ''
    runHook postInstallCheck
  '';

  meta = {
    description = "Portable jj-hooks release binaries (jj-hp pre-push gate)";
    homepage = "https://github.com/mattwilkinsonn/jj-hooks";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "jj-hp";
  };
}
