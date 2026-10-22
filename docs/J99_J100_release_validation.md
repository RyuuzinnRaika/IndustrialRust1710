# J99/J100: Windows/Linux release build and provenance

This workflow is intentionally separate from the normal pull-request CI.

## Run

1. From GitHub Actions, open **Windows and Linux verified binaries** and choose **Run workflow**.
2. Alternatively, push a release tag beginning with `v`. Tags do **not** automatically publish a GitHub Release.
3. Open the successful run's artifacts and obtain the package for your OS.
4. Inspect `build-info.txt`, `Cargo.lock`, `cargo-metadata.json`, and `SHA256SUMS` before executing the server.
5. Verify SHA256 hashes using `sha256sum -c SHA256SUMS` on Linux, or `Get-FileHash -Algorithm SHA256` on Windows (compare to the listed hex digest).

The bundle includes the `vanilla-server` and `legacy-status-server` executable files. To run locally, prepare `server.properties` and the documented whitelist as appropriate. **Do not publicly expose an offline-mode server.**

## What the provenance guarantees

The generated `Cargo.lock` pins dependencies **within a single workflow run**; all compilation/test commands use `--locked`. `cargo-metadata.json` captures the resolved dependency graph. `build-info.txt` captures GitHub commit, ref, toolchain version, platform, and workflow run ID. `SHA256SUMS` checks the uploaded binary and metadata bytes.

**Limitations:** Because the repository does not currently commit a `Cargo.lock`, two independent runs may resolve different dependencies. The `stable` Rust toolchain and hosted runners may also change. This is traceability, not bit-for-bit reproducible-build verification or artifact signing. To strengthen reproducibility, commit a reviewed `Cargo.lock`, pin the Rust toolchain and CI action revisions, and compare independently compiled artifacts. GitHub workflow artifacts expire according to retention settings.

This workflow does not replace J97: Rust TCP integration tests are not equivalent to tests executed by a real Minecraft Java 1.7.10 client.
