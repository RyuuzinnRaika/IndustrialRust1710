# J92–J96 offline storage integration (partial)

These commands **do not** enable live writes into a vanilla Anvil world.

```sh
vanilla-server --check-config [server.properties]
vanilla-server --verify-world ./world/flat.irw
vanilla-server --verify-world ./exported-anvil-world
vanilla-server --prepare-anvil-copy ./vanilla-world ./working-copy
vanilla-server --world ./world/flat.irw --backup-rotate ./backups 7
```

- **J92:** Stop the server before invoking offline `--backup-rotate`. It snapshots with `session.lock`, checks copied files byte-for-byte, runs format-level verification, and deletes **only** its own marked older snapshots. Keep must be 1–100. Scheduling/restart orchestration requires a working graceful `/stop` implementation; it is **not** claimed by this PR.
- **J93:** `--verify-world` loads native IRW and known companion data or bounded Java 1.7.10 Anvil metadata, regions and playerdata **without creating a session lock**. Presence of an unfinished IRW journal is an error. Format checks do not prove gameplay-level consistency of every object.
- **J94:** `--check-config` validates `server.properties` through the existing parser and storage safety guard without starting a server. Offline-mode binding to a non-loopback address fails inspection. Unknown options continue to generate explicit warnings; not all unsupported keys are fatal.
- **J95:** Explicit `level-type=DEFAULT` and `level-seed=...` in `server.properties` invoke the **IRW3 biome world** generator for a NEW survival world; signed longs and Java UTF-16 string-hash seeds are interpreted like Java. Seeds with `FLAT` are rejected, rather than silently ignored. Existing worlds are checked for generation identity; mismatched seeds cause startup failure. This is not bit-identical vanilla world generation.
- **J96:** `--prepare-anvil-copy` validates a 1.7.10 Anvil source and makes a fresh, byte-compared working tree. Never overwrite a destination or convert arbitrary Anvil data to IRW. This is only the safe migration staging step, not a completed generic converter.
- **J97:** Rust TCP integration tests are **not** proof of real Java 1.7.10 GUI behavior; the external-client acceptance harness remains pending.

**Do not run an offline backup while another server is live**. Java servers may not cooperate with advisory OS locks, and a changing source can undermine snapshots. Retain independent backups before migration. Real-client tests and continuous live backup still need follow-up implementation and CI proof.
