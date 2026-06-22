## Release checklist

This document is a checklist for the release process of the `logged-tcp-proxy` project.

- Ensure that the [CHANGELOG.md](./CHANGELOG.md) contains all unreleased changes and adheres to the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.
- Ensure that the [README.md](./README.md) contains all the necessary information about the new version.
- Ensure that the [CONTRIBUTING.md](./CONTRIBUTING.md) is up to date with any new contribution guidelines or processes.
- Ensure that GitHub Actions checks are passing. If the MSRV has changed, update the badge in [README.md](./README.md), add a note to [CHANGELOG.md](./CHANGELOG.md), and update the `rust-version` property in [Cargo.toml](./Cargo.toml).
- Define a new version according to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and update it in the following files:
  - Update the `version` property in [Cargo.toml](./Cargo.toml) to the new version.
  - Rename the `Unreleased` section in [CHANGELOG.md](./CHANGELOG.md) to the new version and current date.
  - Update the tag version in the installation section of [README.md](./README.md) to the new version.
- Rebuild [Cargo.lock](./Cargo.lock) by running `cargo build`.
- Commit changes with message `v<version>`.
- Run `cargo publish` to publish the crate to crates.io.
- Push changes to the repository.
- Draft a new release in the repository's "Releases" section on GitHub. Include the new version and the changelog highlights in the release description.
