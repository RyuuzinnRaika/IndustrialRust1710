# NEXT Issue evidence audit and E42 comparison

This is an **offline verification aid**, not an implementation of Minecraft behavior.
It supports [#1178](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1178),
[#1182](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1182),
and [#1183](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1183).

## A. Audit missing issue and PR evidence

Save complete issue and PR API snapshots, including **closed** entries, to local
JSON arrays. The script never calls the network. For example, a user with
an authorized `gh` CLI installation can export paginated snapshots:

```bash
gh api --paginate --slurp 'repos/RyuuzinnRaika/IndustrialRust1710/issues?state=all&per_page=100' --jq 'add' > issues.json
gh api --paginate --slurp 'repos/RyuuzinnRaika/IndustrialRust1710/pulls?state=all&per_page=100' --jq 'add' > prs.json
python tools/audit_next_evidence.py audit --issues issues.json --prs prs.json --output evidence-gaps.json
```

GitHub's Issues collection also contains PRs; these are excluded from the issue
scan if they have a `pull_request` field. Feed the Pulls collection separately.
The scanner recognizes candidate **named Rust tests**, exact 40-hex SHA strings
and explicitly declared requirement IDs. It does **not** treat words like
`tested` or `PASS` as evidence, and it does not read comments, check-runs,
linked artifacts or the actual commit graph. Its gaps are therefore
**potential documentation/verification gaps**, not definitive proof of missing
tests, nor grounds to close or reopen issues automatically.

No real-client `PASS` is granted from text in a GitHub issue/PR.

## B. Compare local Rust E2E tests and real E42 acceptance

Prepare a JSON mapping with **exactly** the nine keys `42-01` through `42-09`.
Each value is the **actual unique Rust test name** printed by Cargo; do not
invent or reuse names. A case with no matching result is `NOT_RUN`.
Generate the test output on a trusted local development machine:

```bash
cargo test -p vanilla-server --test protocol5_e2e -- --test-threads=1 > protocol5-e2e.log 2>&1
python tools/audit_next_evidence.py ledger \
  --rust-log protocol5-e2e.log --mapping e42-rust-map.json \
  --commit YOUR_EXACT_40_HEX_COMMIT_SHA --output e42-ledger.json
```

When an unmodified Minecraft Java 1.7.10 client's E42 artifact report and
evidence files exist, also pass `--client verified-e42.json
--evidence-root /path/to/evidence`. The original
`tools/e42_acceptance.py` validator checks the E42 report, including sealed
SHA-256 artifacts. The Rust and E42 commit SHA must match.

The output records each case's Rust status, client status, overall status and
commit SHA. The overall status is `PASS` only if **both** verified inputs
report `PASS`. `FAIL` is never hidden; `BLOCKED` becomes `NOT_RUN`
in the client comparison while retaining the original source status.

## Checks / limitations

```bash
python -m unittest discover -s tools -p 'test_audit_next_evidence.py' -v
```

The script is read-only except for exclusive creation of its JSON output.
It never starts GitHub Actions Rust CI, alters a world, changes live
server behavior or uploads local user data. Review exported reports
before sharing them, and keep full raw evidence private when necessary.
