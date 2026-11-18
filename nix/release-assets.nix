# Pinned GitHub Release assets for packages.<system>.jj-hooks-bin. Each hash is
# the SRI form of the asset's published .sha256 sidecar; bump the version and
# convert the three new digests with `nix hash convert --hash-algo sha256 --to sri`.
let
  version = "0.7.0";
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
        hash = "sha256-b+Gu4+OPmjqmsj0hwZrNf4gCc9iQlTh2bXfuzbXQPoE=";
      };
    aarch64-linux =
      let
        slug = "linux-arm64";
      in
      {
        name = "jj-hooks-v${version}-${slug}.tar.gz";
        hash = "sha256-X6ebxSQ/bvpUlOV1rFJGvUUDT7w58CYbi7sITmh7uU8=";
      };
    aarch64-darwin =
      let
        slug = "darwin-arm64";
      in
      {
        name = "jj-hooks-v${version}-${slug}.tar.gz";
        hash = "sha256-ArFKXQhjFkRSGibCdfhJjnF8JUlD4tEdulZIwjFVuQw=";
      };
  };
}
