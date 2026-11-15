# Rust build and PostgreSQL schema

This directory contains the Rust service, the production Docker build, the catalog and document exporters, and the SQL grants for existing installations.

- Start the complete developer stack from the repository root with `./scripts/dev.sh`. It uses a dedicated local PostgreSQL volume, synthetic accounts and a proxy bound to loopback.
- Run the Rust HTTP suite against a fresh Compose project with `./scripts/test-api.sh`.
- Build the Linux production release with `deploy/build-rust-release.sh EMPTY_DIRECTORY`. The production artifact logic remains in the `places` Docker target.
- For an empty PostgreSQL database, the Rust binary's `init-schema` command installs the baseline using the owner's `WIRING_SCHEMA_DATABASE_URL`. Apply the grants appropriate to the service roles before exposing an installation. Existing production databases are managed separately.

The directory name is historical. Development and CI do not access the public beta server.
