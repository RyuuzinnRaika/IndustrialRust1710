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
import time
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen

# Explicitly reviewed, superseded PR heads, replaced by later merged mainline PRs
# or retired as disconnected duplicate implementations. Each pin is immutable.
# Retire ONLY these exact, unchanged heads after their PRs have closed.
# Never infer abandonment from branch age or from closure alone.
RETIRED_PR_HEADS = {
    35: "f3effdddca3ea1829b711fac576d9dc70c7c4ba2",
    38: "f699c71ad6dfe484b504fd6bd6b684dc5be309be",
    42: "99fc2f9c4401521f72a2e42b78b440e55426b934",
    43: "2f4c864579b7e210716be0f5e5cfbcd09d59ce88",
    45: "cd843ea00a92db3276a1554e3779e95fb4c48e9e",
    47: "f3effdddca3ea1829b711fac576d9dc70c7c4ba2",
    48: "c1c75580bbee15887364d7771342c81c26ca7fd7",
    54: "f85561181f63cb51cc25182dc859a7e6263bc582",
    57: "462196d8c3ca0494e7fc33d5268ee26a73bce15f",
    59: "e4794b8dff08c905595a817439f877eabe5f622b",
    61: "fad31e5d09d8d0b163dde8408a084a50577f0b61",
    62: "b706f21f194b27ae6d2053fc43ae33eb5f9f37f7",
    70: "d7867e695c2da9a0a670210f1b4b2ee50da675cd",
    71: "97a89e255a985eed7ce3baf656cf9a27bef581bf",
    75: "0329c5947c1e673f2d3eb4bd19238c896ac4d3ba",
    78: "a56510fdeb5e7e6e6192290803473ee47a1d1012",
    80: "0c44ec15dbb0cd1721ebe4df5dd8e8827f2200aa",
    82: "34d2da82ab60f88e9d3d8a7c87e74bce7220fa5d",
    83: "84450a3ba10c63d4e09393101e102bc4a319449d",
    85: "638679baa5940a3245aebf914a5661072b806a8f",
    87: "82c1f03c43f8e63f3b3dd3598e0495c9ee1f17fa",
    88: "9b7916280f470963497206f42409d6bd5e566389",
    97: "7c5d2b4d7c6bb8f85d5f5fae9ba34c6c8c985b82",
    98: "cc2a74a3ef0f53aebf1146143de6b5525dd6a518",
    117: "f2bd80e5de9d3db14c8046d45221f9ab4a78f07d",
    118: "5d58eca0890cc2719d334015075b3b43c30c7fc9",
    121: "e10fc063689670f47b615e793b496223c3ed3efc",
    122: "03af198341e7857a8a5c54c13bd94fe34571656a",
    139: "4c8296e66055396033ecdc9878c3a59afa220432",
    141: "15e6bea2b632e4421156accbb10c099fc49ff284",
    147: "645f634ac2ff1671538fd97f9e73177cd408dede",
    148: "6de2b50cf89952a9dde3c19df303a902ae26d499",
    164: "6f1abcf6839b0722d2af8fc0a997401ce51d442c",
    167: "b99c9f635d1c1817e7034a5fa87bbb59eacaa515",
    168: "89bb13307864bec1f9dd88a6de4e4fd2c6ca08c5",
    169: "11bfa1d4869dd4e792905f9b54121f9704af488e",
    170: "e501085194be6d1ec0ca989694749614fd9194f7",
    171: "1a1c0c61f5331bf02bafa999766022080a10147c",
    173: "bc368e33d73243e380452e18463b5e21e3e4f409",
    176: "88c5b578794461717e04d572c0cac09db8a285b5",
    352: "86334e801be0bdde11bf01149a195a9e3bfca836",
}

# Nested PRs integrated into the indicated later main PR. All refs are immutable
# SHA-pins and require both PRs to be merged in chronological order.
RETIRED_MERGED_STACK_HEADS = {
    36: ("dfbde540e3466a99e744cee1969d85afd43a8c23", 50),
    39: ("14c8ff46eb414518fc646187cb45bbed2c06d537", 50),
    40: ("2077f22807a379c61b9e04101fd3fd8bc031595d", 50),
    41: ("125b60a2f35ade37ed9a99b704d796f4bc4ce7db", 50),
    69: ("f98adc7f6b95aba57f773c3d6052334c0c052084", 185),
    73: ("031e133e89cf74980d5133e642a80fad05312281", 185),
    124: ("696d14fe217043928dfd631f0d60bbb398089179", 116),
}

# Reviewed B11/B14 stack: #55 first landed on main, then #60 was merged
# back into its parent. Current main has the rail/dust/comparator/door
# regression coverage; never replay this outdated base as a full merge.
RETIRED_LATE_STACK_PAIR = {
    "parent": ("feat/b11-20-block-physics-20261021", "7dfca59caebab75d7a7c470392af09482cc36013", 55),
    "child": ("feat/b11-20-redstone-signal-regression-20261021", "9c068fc7ea13348788f2addec76752831f235bfe", 60),
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
        # Every retired head is explicitly pinned; it may come from a reviewed stacked PR.
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
    for attempt in range(3):
        try:
            with urlopen(request, timeout=30) as response:
                payload = response.read()
                return json.loads(payload) if payload else None
        except HTTPError as exc:
            if exc.code not in (500, 502, 503, 504) or attempt == 2:
                raise
            time.sleep(2 ** attempt)  # Retry transient GitHub API errors only.


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
    # Ignore a closed, unmerged PR's historical head only if the exact live
    # branch tip was independently merged to main by a later PR.
    merged_main_heads = {
        pr["head"]["ref"]: pr["head"]["sha"]
        for pr in prs
        if pr.get("merged_at") and pr["base"]["ref"] == default_branch
        and pr["head"]["repo"] and pr["head"]["repo"]["full_name"] == repo
    }
    for pr in prs:
        if pr["state"] == "open":
            if pr["head"]["repo"] and pr["head"]["repo"]["full_name"] == repo:
                keep.add(pr["head"]["ref"])
            keep.add(pr["base"]["ref"])
        elif not pr.get("merged_at") and not retired_closed_pr(pr, repo):
            head = pr["head"]
            if head["repo"] and head["repo"]["full_name"] == repo:
                live = branches_by_name.get(head["ref"])
                if (live and head["sha"] != live["commit"]["sha"]
                        and merged_main_heads.get(head["ref"]) == live["commit"]["sha"]):
                    continue  # Older abandoned PR, but current tip was merged to main.
                keep.add(head["ref"])

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

    # Source PR merged into a feature branch before a later mainline integration.
    pr_by_number = {pr["number"]: pr for pr in prs}
    for number, (expected_sha, successor_number) in RETIRED_MERGED_STACK_HEADS.items():
        source = pr_by_number.get(number)
        successor = pr_by_number.get(successor_number)
        if not source or not successor:
            continue
        if (not source.get("merged_at") or not successor.get("merged_at")
                or successor["base"]["ref"] != default_branch
                or source["merged_at"] >= successor["merged_at"]):
            continue
        head = source["head"]
        if (not head["repo"] or head["repo"]["full_name"] != repo
                or head["sha"] != expected_sha):
            continue
        name = head["ref"]
        branch = branches_by_name.get(name)
        if (not branch or branch.get("protected") or name in keep
                or branch["commit"]["sha"] != expected_sha):
            continue
        candidates[name] = {"sha": expected_sha, "pr": number}

    # Explicitly reviewed late-merged feature stack, identified by both SHA
    # and relationship between the original main PR and its child PR.
    parent_name, parent_sha, parent_num = RETIRED_LATE_STACK_PAIR["parent"]
    child_name, child_sha, child_num = RETIRED_LATE_STACK_PAIR["child"]
    parent = pr_by_number.get(parent_num)
    child = pr_by_number.get(child_num)
    p_ref = branches_by_name.get(parent_name)
    c_ref = branches_by_name.get(child_name)
    if (parent and child and p_ref and c_ref
            and parent.get("merged_at") and parent["base"]["ref"] == default_branch
            and child.get("merged_at") and child["base"]["ref"] == parent_name
            and parent["head"]["ref"] == parent_name
            and child["head"]["ref"] == child_name
            and child["head"]["sha"] == child_sha
            and child.get("merge_commit_sha") == parent_sha
            and parent["merged_at"] < child["merged_at"]
            and p_ref["commit"]["sha"] == parent_sha
            and c_ref["commit"]["sha"] == child_sha
            and not p_ref.get("protected") and not c_ref.get("protected")
            and parent_name not in keep and child_name not in keep):
        candidates[parent_name] = {"sha": parent_sha, "pr": parent_num}
        candidates[child_name] = {"sha": child_sha, "pr": child_num}

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
            if e.code in (500, 502, 503, 504):
                print("SKIP (transient GitHub failure):", name, e.code)
                continue
            print("ERROR:", name, e.code, file=sys.stderr)
            raise
    if not args.apply:
        print("No branches were deleted (dry run).")


if __name__ == "__main__":
    main()
