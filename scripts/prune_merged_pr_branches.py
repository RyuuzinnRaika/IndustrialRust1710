#!/usr/bin/env python3
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
        elif not pr.get("merged_at"):
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

    print("Repository:", repo)
    print("Branches:", len(branches), "PRs:", len(prs))
    print("Verified main-merged candidates:", len(candidates))
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
