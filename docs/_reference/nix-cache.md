---
title: Nix Binary Cache
description: Status and maintainer setup for publishing Nix builds.
---

An official public binary cache is not active yet. Nix installations may still
build Spotizgeg from source. The GitHub Actions cache speeds up CI only; it is
not a public Nix substituter.

## Maintainer setup

1. Create a public cache in a maintainer-owned
   [Cachix account](https://app.cachix.org/). Use Cachix-managed signing and
   create a token with permission to push to that cache.
2. Add the token as the GitHub repository secret `CACHIX_AUTH_TOKEN`. Do not
   put it in an issue, configuration file, or public documentation.
3. Set the repository variable `CACHIX_CACHE` to the cache's name. Leaving
   this variable unset keeps the existing CI build and cache behavior.
4. Push a checked change to `main`. After `nix build .#default` succeeds,
   the Nix CI job publishes the result and its dependency closure. A configured
   cache without its upload token fails publication instead of claiming success.
5. Verify the uploaded package can be substituted on a separate Nix machine.
   Then replace the pending notice here, and add to the Download page, the actual
   cache URL, public signing key, and user setup instructions from Cachix.

Pull requests and manual workflow runs only read the public cache. The upload
token is supplied only to the publication step on a push to `main`.

The current Nix job builds `x86_64-linux`. It does not populate macOS or ARM64
packages. A different flake revision, system, or build override can still need
a local build even after the cache is active. This cache does not publish a
GitHub application release or change its download artifacts.

See the [Cachix action documentation](https://github.com/cachix/cachix-action)
for how the cache is added as a substituter. The workflow uses the action only
for read access and explicitly pushes the successful package closure afterward.
