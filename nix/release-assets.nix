# Pinned GitHub Release assets for packages.<system>.jj-hooks-bin. Each hash is
# the SRI form of the asset's published .sha256 sidecar; bump the version and
# convert the three new digests with `nix hash convert --hash-algo sha256 --to sri`.
let
  version = "0.6.0";
in
{
  inherit version;
  assets = {
    x86_64-linux =
      let
        slug = "linux-x64";
      in
      {
        name = "jj-hooks-v${version}-${slug}.tar.gz";
        hash = "sha256-PL9xtMrRjyMtmyg68eyMOrbHP4ZXF7/oJFNtrwBsYhQ=";
      };
    aarch64-linux =
      let
        slug = "linux-arm64";
      in
      {
        name = "jj-hooks-v${version}-${slug}.tar.gz";
        hash = "sha256-dcHEXW7E9kY5nMQVFOrgWwwAGqajxenslYzbnMX4sJI=";
      };
    aarch64-darwin =
      let
        slug = "darwin-arm64";
      in
      {
        name = "jj-hooks-v${version}-${slug}.tar.gz";
        hash = "sha256-NXBXBOnhYYB8lffzO/4YbbLxqVf1tYDfCxEdwMNzyDk=";
      };
  };
}
