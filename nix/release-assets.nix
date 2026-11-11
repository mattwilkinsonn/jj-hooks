# Pinned GitHub Release assets for packages.<system>.jj-hooks-bin. Each hash is
# the SRI form of the asset's published .sha256 sidecar; bump the version and
# convert the three new digests with `nix hash convert --hash-algo sha256 --to sri`.
{
  version = "0.6.0";
  assets = {
    x86_64-linux = {
      name = "jj-hooks-v0.6.0-linux-x64.tar.gz";
      hash = "sha256-PL9xtMrRjyMtmyg68eyMOrbHP4ZXF7/oJFNtrwBsYhQ=";
    };
    aarch64-linux = {
      name = "jj-hooks-v0.6.0-linux-arm64.tar.gz";
      hash = "sha256-dcHEXW7E9kY5nMQVFOrgWwwAGqajxenslYzbnMX4sJI=";
    };
    aarch64-darwin = {
      name = "jj-hooks-v0.6.0-darwin-arm64.tar.gz";
      hash = "sha256-NXBXBOnhYYB8lffzO/4YbbLxqVf1tYDfCxEdwMNzyDk=";
    };
  };
}
