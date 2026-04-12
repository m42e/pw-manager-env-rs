#!/usr/bin/env python3
"""Update GitHub Actions workflow pins to their latest commit SHAs."""

import json
import re
import sys
from pathlib import Path
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS_DIR = ROOT / ".github" / "workflows"

USES_LINE_PATTERN = re.compile(r'^(\s*(?:-\s+)?uses:\s*)([^@\s]+)@([^\s#]+)(?:\s+(#.*))?$')
PIN_COMMENT_PATTERN = re.compile(r'(?:^|\s)#\s*pin:\s*([^\s#]+)')

GITHUB_HEADERS = {
    "Accept": "application/vnd.github+json",
    "User-Agent": "pw-env-workflow-action-updater",
}


def is_commit_sha(value):
    return bool(re.match(r'^[0-9a-f]{40}$', value, re.IGNORECASE))


def parse_uses_line(line):
    match = USES_LINE_PATTERN.match(line)
    if not match:
        return None

    prefix, repo, ref = match.group(1), match.group(2), match.group(3)
    comment = match.group(4) or ""

    if repo.startswith("./") or repo.startswith("docker://") or len(repo.split("/")) != 2:
        return None

    pin_match = PIN_COMMENT_PATTERN.search(comment)
    if pin_match:
        tracked_ref = pin_match.group(1)
    elif not is_commit_sha(ref):
        tracked_ref = ref
    else:
        tracked_ref = None

    return {
        "prefix": prefix,
        "repo": repo,
        "ref": ref,
        "tracked_ref": tracked_ref,
        "has_pin_comment": bool(pin_match),
    }


def github_get(url):
    req = Request(url, headers=GITHUB_HEADERS)
    with urlopen(req) as resp:
        return json.loads(resp.read().decode())


def resolve_commit_sha(repo, ref, cache):
    cache_key = f"{repo}@{ref}"
    if cache_key in cache:
        return cache[cache_key]

    url = f"https://api.github.com/repos/{repo}/commits/{quote(ref, safe='')}"
    try:
        payload = github_get(url)
    except HTTPError as e:
        raise RuntimeError(f"GitHub API returned {e.code} for {repo}@{ref}") from e

    sha = payload.get("sha", "")
    if not is_commit_sha(sha):
        raise RuntimeError(f"GitHub API did not return a commit SHA for {repo}@{ref}")

    cache[cache_key] = sha
    return sha


def resolve_latest_tracked_ref(repo, current_tracked_ref, cache):
    if repo in cache:
        return cache[repo]

    url = f"https://api.github.com/repos/{repo}/releases/latest"
    try:
        payload = github_get(url)
    except HTTPError as e:
        if e.code == 404:
            cache[repo] = current_tracked_ref
            return current_tracked_ref
        raise RuntimeError(f"GitHub API returned {e.code} for latest release of {repo}") from e

    tag_name = payload.get("tag_name", "")
    result = tag_name if isinstance(tag_name, str) and tag_name else current_tracked_ref
    cache[repo] = result
    return result


def build_updated_line(prefix, repo, sha, tracked_ref):
    return f"{prefix}{repo}@{sha} # pin: {tracked_ref}"


def shorten_sha(sha):
    return sha[:7]


def main():
    args = set(sys.argv[1:])
    force_write = "--write" in args or "-w" in args
    check_only = "--check" in args
    interactive = not force_write and not check_only and sys.stdin.isatty() and sys.stdout.isatty()

    workflow_entries = sorted(
        e for e in WORKFLOWS_DIR.iterdir()
        if e.suffix in (".yml", ".yaml")
    )

    files = {}   # Path -> list of lines
    groups = {}  # groupKey -> group dict

    for entry in workflow_entries:
        content = entry.read_text(encoding="utf-8")
        lines = content.split("\n")
        files[entry] = lines

        for line_index, line in enumerate(lines):
            parsed = parse_uses_line(line)
            if not parsed or not parsed["tracked_ref"]:
                continue

            group_key = f"{parsed['repo']}@{parsed['tracked_ref']}"
            occurrence = {"entry": entry.name, "file_path": entry, "line_index": line_index, **parsed}

            if group_key not in groups:
                groups[group_key] = {
                    "repo": parsed["repo"],
                    "tracked_ref": parsed["tracked_ref"],
                    "occurrences": [],
                }
            groups[group_key]["occurrences"].append(occurrence)

    if not groups:
        print("No external GitHub Actions were found in .github/workflows.")
        sys.exit(0)

    sha_cache = {}
    release_tag_cache = {}

    for group in groups.values():
        group["latest_tracked_ref"] = resolve_latest_tracked_ref(
            group["repo"], group["tracked_ref"], release_tag_cache
        )
        group["latest_sha"] = resolve_commit_sha(
            group["repo"], group["latest_tracked_ref"], sha_cache
        )
        group["current_refs"] = list(dict.fromkeys(o["ref"] for o in group["occurrences"]))
        group["needs_update"] = any(
            o["ref"] != group["latest_sha"]
            or o["tracked_ref"] != group["latest_tracked_ref"]
            or not o["has_pin_comment"]
            for o in group["occurrences"]
        )

    outdated_groups = [g for g in groups.values() if g["needs_update"]]
    up_to_date_groups = [g for g in groups.values() if not g["needs_update"]]

    for group in up_to_date_groups:
        print(f"up-to-date  {group['repo']}@{group['latest_tracked_ref']} -> {shorten_sha(group['latest_sha'])}")

    for group in outdated_groups:
        current_refs = ", ".join(group["current_refs"])
        count = len(group["occurrences"])
        uses_str = "use" if count == 1 else "uses"
        print(
            f"update      {group['repo']}@{group['tracked_ref']} {current_refs} -> "
            f"{group['latest_tracked_ref']} {shorten_sha(group['latest_sha'])} ({count} {uses_str})"
        )

    if not outdated_groups:
        print("All tracked workflow action pins are current.")
        sys.exit(0)

    if check_only:
        sys.exit(1)

    apply_all = force_write
    write_count = 0

    try:
        for group in outdated_groups:
            should_apply = force_write

            if not force_write and interactive:
                if not apply_all:
                    answer = input(
                        f"Update {group['repo']} from {group['tracked_ref']} to "
                        f"{group['latest_tracked_ref']} ({group['latest_sha']})? [Y]es/[n]o/[a]ll/[q]uit "
                    ).strip().lower()

                    if answer == "q":
                        break
                    elif answer == "a":
                        apply_all = True
                        should_apply = True
                    else:
                        should_apply = answer in ("", "y", "yes")
                else:
                    should_apply = True

            if not should_apply:
                continue

            for occurrence in group["occurrences"]:
                lines = files[occurrence["file_path"]]
                lines[occurrence["line_index"]] = build_updated_line(
                    occurrence["prefix"],
                    occurrence["repo"],
                    group["latest_sha"],
                    group["latest_tracked_ref"],
                )
                write_count += 1
    except KeyboardInterrupt:
        pass

    if write_count == 0:
        msg = (
            "No workflow action pins were changed."
            if interactive
            else "Run with --write to apply the available updates."
        )
        print(msg)
        sys.exit(0)

    for file_path, lines in files.items():
        file_path.write_text("\n".join(lines), encoding="utf-8")

    uses_str = "use" if write_count == 1 else "uses"
    print(f"Updated {write_count} workflow action {uses_str}.")


if __name__ == "__main__":
    main()
