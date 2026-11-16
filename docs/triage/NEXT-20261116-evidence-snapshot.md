# NEXT-001–200 evidence snapshot (issue-body-only)

Observed: 2026-11-16 JST, from the GitHub Issues REST collection
(`state=all`, paginated), excluding entries that are pull requests.

**This is a metadata audit, not a claim that closed Issues failed their acceptance
criteria.** It does not inspect issue comments, linked PR reviews, checks,
commit history or artifacts.

## Count

| Metric | Count |
|---|---:|
| NEXT-series issues inspected | 200 |
| Open | 170 |
| Closed | 30 |
| Issue bodies without an exact 40-hex commit SHA candidate | 200 |
| Issue bodies without a Rust-style `module::test` name candidate | 199 |
| Issue bodies without either text pattern | 199 |
| Issue bodies with an E42 / 42-0N reference | 0 |

Pattern matches alone are **not proof** of verified acceptance. Equally, no
pattern in an Issue body does **not** mean the work is missing: discussions,
PRs, local reports and external evidence may hold it.

## Closed issue IDs requiring cross-source verification

`#1176, #1175, #1174, #1155, #1154, #1147, #1146, #1145, #1144, #1134,
#1104, #1094, #1084, #1076, #1075, #1074, #1054, #1029, #1024, #1014,
#1004, #1000, #997, #995, #994, #992, #989, #986, #985, #984`

Before considering a reopened/closed state change, check linked PR, exact
test names and environment, a corresponding commit SHA, and separate client
E42 evidence where applicable. Do not assume every issue requires E42;
internal-only checks may be not applicable.

## Regenerating the audit

Use `tools/audit_next_evidence.py audit` with complete GitHub JSON snapshots
and the instructions in `NEXT-evidence-audit-howto.md`. The tool is
read-only and defaults unverified real-client acceptance to `NOT_RUN`.

This snapshot was inspected against main
`686ec7815bcd22fffb245feae671ab61f357ecea`. It is time-specific and
must not be reused as a current status report after new changes.
