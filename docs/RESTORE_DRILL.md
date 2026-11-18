# Offline restore drill (experimental)

`restore-drill` is a separate Rust verification utility, not a supervisor or a replacement for the existing backup and administration paths. Build it together with `vanilla-server`, then run:

```text
restore-drill --server PATH_TO_VANILLA_SERVER --backup BACKUP_DIR --scratch-root PRIVATE_SCRATCH_DIR [--protect-world WORLD_PATH]... [--timeout-seconds 120]
```

The server, backup, scratch root, and protected paths must already exist. `--protect-world` can be repeated for known live world directories or IRW files; a protected file causes its entire parent directory to be protected, including sidecars and players. The backup must contain the exact IndustrialRust1710 backup marker and either one top-level `.irw` file or an Anvil `level.dat`, not both. Unsupported links, Windows reparse points, ambiguous layouts, parent traversal and overlapping paths are rejected. The verifier has bounded entry count, depth, path storage, file count and total file bytes; oversized backups are rejected.

Choose a private scratch root outside *all* live worlds, backups, and other valuable data. List every known live world with `--protect-world`; the program cannot discover unknown worlds, mount aliases, or other processes' data. Use only a trusted server executable and a stopped, stable backup. The operator must control the scratch root and all relevant parent directories and prevent concurrent replacement or modification throughout the run. Path and content checks do not eliminate filesystem time-of-check/time-of-use races, hard-link aliases, separate mount aliases, or changes that occur between observations.

On Windows, containment checks for alternative UNC names and Unicode or case aliases have not been validated across filesystems. Do not use alternate paths or share/mount aliases to refer to the same data; keep scratch physically separate from all worlds and backups. Linux execution has not yet been verified.

The utility snapshots names, file lengths and SHA-256 contents before copying into a unique child of the chosen scratch root. It compares both the source and copy after copying and again after the dedicated native `--verify-world` process succeeds. The child has a timeout; only that dedicated child is stopped and reaped. The timeout is best-effort: scheduling may delay observation, and waiting after a successful kill has no separate deadline. On success the owned scratch directory is removed. Before cleanup, errors leave the scratch directory in place, including when child termination cannot be confirmed. If cleanup itself fails, contents may be partly removed or their state unknown; the path and observed state are reported. Inspect it and ensure the child has stopped before handling it. A success reports the checks performed on that stopped copy, not full compatibility or a production cutover.

The comparison does not cover access times, permissions or other file metadata; the operating system may update access times during reads. The input limits and child timeout do not impose a separate memory limit on the native verifier.

The Windows junction regression is ignored in the ordinary suite because creating junctions may require additional permission. Run it separately on a suitable development host with `cargo test -p vanilla-server --bin restore-drill snapshot::tests::windows_junctions_are_rejected_at_root_and_inside_backup -- --exact --ignored` and record the result; an ignored test is not a pass.
