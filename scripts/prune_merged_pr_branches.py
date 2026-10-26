#!/usr/bin/env python3
# Maintenance: branch-prune run requested on 2026-10-26 JST; safeguards unchanged.
"""Conservatively prune repository branches whose PRs were merged into main.

Default mode only prints candidates. Run with --apply to delete them.
Requires GITHUB_TOKEN with contents:write for --apply.
"""
import argparse
import json
import os
import sys
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen

# These three long-lived PRs were superseded by later, independently reviewed
# mainline integrations. Retire ONLY these exact, unchanged head commits,
# and ONLY after their PRs have been explicitly closed without merging.
# Never infer abandonment from the age or closure of any other PR.
RETIRED_PR_HEADS = {
    61: "fad31e5d09d8d0b163dde8408a084a50577f0b61",
    141: "15e6bea2b632e4421156accbb10c099fc49ff284",
    171: "1a1c0c61f5331bf02bafa999766022080a10147c",
}


def retired_closed_pr(pr, repo):
    expected = RETIRED_PR_HEADS.get(pr["number"])
    head = pr["head"]
    return (
        expected is not None
        and pr["state"] == "closed"
        and not pr.get("merged_at")
        and head["repo"] is not None
        and head["repo"]["full_name"] == repo
        and head["sha"] == expected
        and pr["base"]["ref"] == "main"
    )


def api(repo, path, method="GET"):
    token = os.environ.get("GITHUB_TOKEN", "")
    headers = {
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "IndustrialRust1710-merged-branch-pruner",
    }
    if token:
        headers["Authorization"] = "Bearer " + token
    request = Request("https://api.github.com/repos/" + repo + path,
                      headers=headers, method=method)
    with urlopen(request, timeout=30) as response:
        payload = response.read()
        return json.loads(payload) if payload else None


def pages(repo, path):
    result = []
    for page in range(1, 31):
        suffix = ("&" if "?" in path else "?") + "per_page=100&page=" + str(page)
        batch = api(repo, path + suffix)
        if not isinstance(batch, list):
            raise RuntimeError("Expected paginated JSON list")
        result.extend(batch)
        if len(batch) < 100:
            return result
    raise RuntimeError("Pagination exceeded safety limit")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""))
    parser.add_argument("--apply", action="store_true", help="Delete verified merged branches")
    args = parser.parse_args()
    repo = args.repo
    if repo != "RyuuzinnRaika/IndustrialRust1710":
        parser.error("Refusing an unexpected repository: " + repo)
    if args.apply and not os.environ.get("GITHUB_TOKEN"):
        parser.error("GITHUB_TOKEN is required for --apply")

    repository = api(repo, "")
    default_branch = repository["default_branch"]
    if default_branch != "main":
        raise RuntimeError("Default branch changed; refusing to prune")

    prs = pages(repo, "/pulls?state=all")
    branches = pages(repo, "/branches")
    branches_by_name = {b["name"]: b for b in branches}

    # Protect every open PR head/base and every head whose work was closed
    # without merging. Do not erase abandoned-but-unintegrated work.
    keep = {default_branch}
    for pr in prs:
        if pr["state"] == "open":
            if pr["head"]["repo"] and pr["head"]["repo"]["full_name"] == repo:
                keep.add(pr["head"]["ref"])
            keep.add(pr["base"]["ref"])
        elif not pr.get("merged_at") and not retired_closed_pr(pr, repo):
            if pr["head"]["repo"] and pr["head"]["repo"]["full_name"] == repo:
                keep.add(pr["head"]["ref"])

    candidates = {}
    for pr in prs:
        if not pr.get("merged_at") or pr["base"]["ref"] != default_branch:
            continue
        head = pr["head"]
        if not head["repo"] or head["repo"]["full_name"] != repo:
            continue
        name = head["ref"]
        b = branches_by_name.get(name)
        if name in keep or b is None or b.get("protected"):
            continue
        if b["commit"]["sha"] != head["sha"]:
            continue  # The branch has moved since this PR was merged.
        candidates[name] = {"sha": head["sha"], "pr": pr["number"]}

    # Exactly specified closed/superseded PR refs may also be retired. They
    # remain protected by every other open or unmerged PR using the same ref.
    for pr in prs:
        if not retired_closed_pr(pr, repo):
            continue
        head = pr["head"]
        name = head["ref"]
        branch = branches_by_name.get(name)
        if name in keep or branch is None or branch.get("protected"):
            continue
        if branch["commit"]["sha"] != head["sha"]:
            continue
        candidates[name] = {"sha": head["sha"], "pr": pr["number"]}

    # Also prune refs whose exact tip is reachable from main. This catches
    # squash-independent branches and obsolete refs without an associated PR.
    # An open PR head/base remains protected even when its tip is an ancestor.
    open_refs = {default_branch}
    for pr in prs:
        if pr["state"] == "open":
            open_refs.add(pr["base"]["ref"])
            head = pr["head"]
            if head["repo"] and head["repo"]["full_name"] == repo:
                open_refs.add(head["ref"])

    def compare_main_ancestry(name, expected_sha):
        path = "/compare/" + quote(name, safe="") + "..." + quote(default_branch, safe="")
        comparison = api(repo, path)
        merge_base = comparison.get("merge_base_commit") or {}
        return (comparison.get("behind_by") == 0
                and comparison.get("status") in ("ahead", "identical")
                and merge_base.get("sha") == expected_sha)

    for name, branch in branches_by_name.items():
        if name in candidates or name in open_refs or branch.get("protected"):
            continue
        head_sha = branch["commit"]["sha"]
        try:
            fully_contained = compare_main_ancestry(name, head_sha)
        except HTTPError as e:
            print("SKIP (ancestry API unavailable):", name, e.code)
            continue
        if fully_contained:
            candidates[name] = {"sha": head_sha, "pr": "ancestor-of-main", "ancestor": True}

    print("Repository:", repo)
    print("Branches:", len(branches), "PRs:", len(prs))
    print("Verified merged or explicitly retired candidates:", len(candidates))
    for name, info in sorted(candidates.items()):
        print(("DELETE" if args.apply else "DRY RUN"), name, "PR", info["pr"])
        if not args.apply:
            continue
        # Recheck just before deleting; a concurrent push must be skipped.
        try:
            current = api(repo, "/branches/" + quote(name, safe=""))
            if current.get("protected") or current["commit"]["sha"] != info["sha"]:
                print("SKIP (protected or branch changed):", name)
                continue
            if info.get("ancestor") and not compare_main_ancestry(name, info["sha"]):
                print("SKIP (no longer an ancestor of main):", name)
                continue
            api(repo, "/git/refs/heads/" + quote(name, safe="/"), method="DELETE")
            print("DELETED:", name)
        except HTTPError as e:
            if e.code == 404:
                print("SKIP (already missing):", name)
                continue
            print("ERROR:", name, e.code, file=sys.stderr)
            raise
    if not args.apply:
        print("No branches were deleted (dry run).")


if __name__ == "__main__":
    main()
