{
  autoPatchelfHook,
  fetchurl,
  lib,
  stdenv,
}:
let
  release = import ./release-assets.nix;
  system = stdenv.hostPlatform.system;
  asset =
    release.assets.${system}
      or (throw "jj-hooks-bin: no v${release.version} release asset for ${system}");
in
stdenv.mkDerivation {
  pname = "jj-hooks-bin";
  version = release.version;
  src = fetchurl {
    url = "https://github.com/mattwilkinsonn/jj-hooks/releases/download/v${release.version}/${asset.name}";
    inherit (asset) name hash;
  };

  # Linux release binaries need their ELF interpreter rewritten to the Nix
  # loader and libgcc_s.so.1 resolved. macOS release binaries are used as-is.
  nativeBuildInputs = lib.optionals stdenv.hostPlatform.isLinux [ autoPatchelfHook ];
  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [ stdenv.cc.cc.lib ];

  dontConfigure = true;
  dontBuild = true;
  # Stripping can invalidate the ad-hoc signed Mach-O release binary.
  dontStrip = true;

  installPhase = ''
    runHook preInstall
    install -Dm755 jj-hooks "$out/bin/jj-hooks"
    install -Dm755 jj-hp "$out/bin/jj-hp"
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    # Capture rather than pipe, so a failing binary fails the build directly.
    hooksVersion="$("$out/bin/jj-hooks" --version)"
    test "$hooksVersion" = "jj-hooks ${release.version}"
    gateVersion="$("$out/bin/jj-hp" --version)"
    test "$gateVersion" = "jj-hp ${release.version}"
    runHook postInstallCheck
  '';

  meta = {
    description = "Prebuilt jj-hooks release binaries (jj-hp pre-push gate)";
    homepage = "https://github.com/mattwilkinsonn/jj-hooks";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "jj-hp";
    platforms = builtins.attrNames release.assets;
    sourceProvenance = with lib.sourceTypes; [ binaryNativeCode ];
  };
}
