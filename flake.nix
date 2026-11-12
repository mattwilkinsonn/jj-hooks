{
  description = "Jujutsu hooks and jj-hp push gate";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixpkgs-darwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
    flake-utils.url = "github:numtide/flake-utils";
    hk.url = "github:jdx/hk/v1.48.0";
    hk.inputs.nixpkgs.follows = "nixpkgs";
    hk-darwin.url = "github:jdx/hk/v1.48.0";
    hk-darwin.inputs.nixpkgs.follows = "nixpkgs-darwin";
  };

  outputs =
    {
      nixpkgs,
      nixpkgs-darwin,
      flake-utils,
      hk,
      hk-darwin,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      packageFor =
        pkgs: system: hkPkgs:
        pkgs.callPackage ./nix/package.nix {
          hk = hkPkgs.packages.${system}.hk;
        };
    in
    {
      overlays.default =
        final: _prev:
        let
          system = final.stdenv.hostPlatform.system;
          hkFlake = if system == "x86_64-darwin" then hk-darwin else hk;
        in
        {
          jj-hooks = packageFor final system hkFlake;
        };
    }
    // flake-utils.lib.eachSystem systems (
      system:
      let
        hkFlake = if system == "x86_64-darwin" then hk-darwin else hk;
        pkgs = import (if system == "x86_64-darwin" then nixpkgs-darwin else nixpkgs) { inherit system; };
        jjHooks = packageFor pkgs system hkFlake;
        jjHooksBin = pkgs.callPackage ./nix/package-bin.nix { };
        hasReleaseBin = builtins.hasAttr system (import ./nix/release-assets.nix).assets;
        app = binary: {
          type = "app";
          program = "${jjHooks}/bin/${binary}";
        };
      in
      {
        packages = {
          jj-hooks = jjHooks;
          default = jjHooks;
        }
        // nixpkgs.lib.optionalAttrs hasReleaseBin {
          jj-hooks-bin = jjHooksBin;
        };

        apps = {
          default = app "jj-hp";
          jj-hp = app "jj-hp";
          jj-hooks = app "jj-hooks";
        };

        # The package derivation runs the Cargo suite and then smoke-tests both
        # installed binaries with --help and --version.
        checks.package = jjHooks;
      }
    );
}
