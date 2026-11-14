#!/usr/bin/env python3
"""Nothing derived from a game, no key and no secret in the repository.

    python scripts/check-content.py              every file git tracks
    python scripts/check-content.py FILE...      only these files

Exit code 0: no finding. 1: findings, one per line as `path:line: reason`. 2: the check could not run.

What it refuses (the rules and the reviewed files are in content-rules.json next to this script):
  - game file formats, the names of game modules and of files derived from a player's game, and folders where
    derived files live (extraction, generated code, builds, saves, caches);
  - any file carrying the code generator's marker (generated code is derived from the game);
  - binary files and images that are not reviewed: each reviewed one is pinned by its SHA-256 with what it is, and
    archives are pinned by the manifest that ships with them;
  - files over the size limit, unless listed;
  - console key material (key-file lines, key names followed by hexadecimal), private keys, access tokens of common
    services, and paths into a real user's profile folder.
It does not judge names and trademarks, licenses or contact data: reviewers do.
Standard library only, Python 3.8 or newer.
"""
import hashlib
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SNIFF = 8192


def glob_match(path, pattern):
    rx, i = "", 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            rx += "(?:.*/)?"
            i += 3
        elif pattern.startswith("**", i):
            rx += ".*"
            i += 2
        else:
            c = pattern[i]
            rx += "[^/]*" if c == "*" else "[^/]" if c == "?" else re.escape(c)
            i += 1
    return re.match(rx + r"\Z", path, re.I) is not None


def any_glob(path, patterns):
    return any(glob_match(path, p) for p in patterns)


def main(argv):
    root = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, cwd=HERE).stdout.strip()
    with open(os.path.join(HERE, "content-rules.json"), encoding="utf-8") as f:
        rules = json.load(f)
    if argv:
        files = [os.path.relpath(os.path.abspath(a), root).replace("\\", "/") for a in argv]
    else:
        out = subprocess.run(["git", "-C", root, "ls-files", "-z"], capture_output=True)
        if out.returncode != 0:
            raise RuntimeError(out.stderr.decode(errors="replace"))
        files = [p for p in out.stdout.decode("utf-8").split("\0") if p]

    marker = "".join(rules["generated_marker_parts"]).encode()
    reviewed = {r["path"]: r for r in rules["reviewed_files"]}
    manifested = {r["path"]: r for r in rules.get("manifested_archives", [])}
    key_line = re.compile(r"^\s*[A-Za-z0-9_]*(?:key|kek|seed)[A-Za-z0-9_]*\s*=\s*[0-9A-Fa-f]{32,}\s*$", re.I)
    console_key = re.compile(rules["console_key_names"] + r".{0,80}?\b[0-9A-Fa-f]{32,}\b", re.I)
    pem = re.compile(r"-----BEGIN (?:[A-Z]+ )*PRIVATE KEY-----")
    tokens = [(n, re.compile(x)) for n, x in rules["token_patterns"].items()]
    user_path = re.compile(r"(?:[A-Za-z]:[\\/]+|\\\\)Users[\\/]+([^\\/\"'`<>|\s:*?]+)", re.I)
    placeholders = {p.lower() for p in rules["placeholder_users"]}
    allowed = rules.get("allowed_lines", [])
    findings = []

    def ok_line(rel, line):
        return any(glob_match(rel, a["path"]) and a["contains"] in line for a in allowed)

    for rel in files:
        if any_glob(rel, rules.get("skip", [])):
            continue
        name = rel.rsplit("/", 1)[-1].lower()
        suffix = name[name.rfind("."):] if "." in name[1:] else ""
        if suffix in rules["blocked_suffixes"]:
            findings.append(f"{rel}:0: file type {suffix} is a game or console format")
        if name in rules["blocked_names"]:
            findings.append(f"{rel}:0: the name of a game module or of a file derived from a player's game")
        bad_dirs = [d for d in rel.lower().split("/")[:-1] if d in rules["blocked_dirs"]]
        if bad_dirs:
            findings.append(f"{rel}:0: inside a '{bad_dirs[0]}' folder, where derived files live")
        path = os.path.join(root, rel)
        if not os.path.isfile(path):
            continue
        with open(path, "rb") as f:
            data = f.read()
        if marker in data:
            findings.append(f"{rel}:0: carries the code generator's marker (generated from game code)")
        binary = b"\0" in data[:SNIFF] or suffix in rules["image_suffixes"]
        digest = hashlib.sha256(data).hexdigest()
        if rel in manifested:
            m = manifested[rel]
            try:
                with open(os.path.join(root, m["manifest"]), encoding="utf-8") as f:
                    want = json.load(f)[m.get("field", "sha256")]
            except (OSError, ValueError, KeyError) as e:
                want = f"unreadable manifest ({e})"
            if want != digest:
                findings.append(f"{rel}:0: SHA-256 {digest[:16]}... differs from {m['manifest']}")
        elif binary:
            r = reviewed.get(rel)
            if r is None:
                findings.append(f"{rel}:0: binary file or image not reviewed (sha256 {digest}): add it to "
                                f"content-rules.json with what it is, once reviewed")
            elif r["sha256"] != digest:
                findings.append(f"{rel}:0: reviewed file changed since its review (sha256 {digest})")
        if len(data) > rules["max_file_bytes"] and rel not in manifested and not any_glob(rel, rules.get("large_files", [])):
            findings.append(f"{rel}:0: {len(data) // 1024} KiB, over the {rules['max_file_bytes'] // 1024} KiB limit")
        if binary:
            continue
        text = data.decode("utf-8", errors="replace")
        for n, line in enumerate(text.splitlines(), 1):
            if ok_line(rel, line):
                continue
            if key_line.match(line):
                findings.append(f"{rel}:{n}: a line in console key-file format")
            if console_key.search(line):
                findings.append(f"{rel}:{n}: a console key name followed by a hexadecimal value")
            if pem.search(line):
                findings.append(f"{rel}:{n}: a private key")
            for tname, rx in tokens:
                if rx.search(line):
                    findings.append(f"{rel}:{n}: a {tname} token")
            for m in user_path.finditer(line):
                who = m.group(1).rstrip(".,;)")
                if who.lower() not in placeholders and not who.startswith(("%", "$", "<", "{")):
                    findings.append(f"{rel}:{n}: path into the profile folder of a user '{who}'")
    for f in findings:
        print(f)
    print(f"check-content: {len(findings)} finding(s) in {len(files)} file(s)", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Exception as e:  # noqa: BLE001 - any failure of the check itself is exit code 2
        print(f"check-content: could not run: {e}", file=sys.stderr)
        sys.exit(2)
