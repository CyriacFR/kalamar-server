#!/usr/bin/env python3
"""Inventory of the third-party packages the lock files pin, with their licenses.

    python scripts/third-party-inventory.py              writes the inventory (third-party.json says where)
    python scripts/third-party-inventory.py --check      exit 1 if the file is out of date or a license is not allowed
    python scripts/third-party-inventory.py --offline    the same without network (cargo uses its local cache)

What it reads, from scripts/third-party.json: Cargo workspaces (through `cargo metadata --locked`, filtered to the
platforms the project builds for) and npm lock files (`package-lock.json`, version 2 or 3). Each package is listed once
per section with its version, its license as the package declares it, and how it is used: linked into the program
("runtime"), only at build time ("build"), or only for tests and tools ("dev").

A license is allowed when its SPDX expression can be satisfied with the licenses of the "allowed" list (for
"A OR B" one of them is enough, for "A AND B" both are needed). A package without a license field, or with one outside
the list, makes --check fail until someone reviews it and either changes the dependency or extends the list with a
reason in third-party.json.
Standard library only, Python 3.8 or newer.
"""
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def spdx_ok(expr, allowed):
    """True when the SPDX expression can be satisfied with allowed licenses. '/' is read as OR (old crates use it)."""
    tokens = re.findall(r"\(|\)|[^\s()]+", expr.replace("/", " OR "))
    pos = 0

    def parse_or():
        nonlocal pos
        v = parse_and()
        while pos < len(tokens) and tokens[pos].upper() == "OR":
            pos += 1
            v = parse_and() or v
        return v

    def parse_and():
        nonlocal pos
        v = parse_atom()
        while pos < len(tokens) and tokens[pos].upper() == "AND":
            pos += 1
            v = parse_atom() and v
        return v

    def parse_atom():
        nonlocal pos
        if pos >= len(tokens):
            raise ValueError(f"incomplete license expression: {expr}")
        t = tokens[pos]
        pos += 1
        if t == "(":
            v = parse_or()
            if pos >= len(tokens) or tokens[pos] != ")":
                raise ValueError(f"unbalanced license expression: {expr}")
            pos += 1
            return v
        if pos < len(tokens) and tokens[pos].upper() == "WITH":
            t = f"{t} WITH {tokens[pos + 1]}"
            pos += 2
        return t in allowed

    try:
        ok = parse_or()
        return ok and pos == len(tokens)
    except (ValueError, IndexError):
        return False


def cargo_packages(entry, offline):
    """(name, version, license, use) of every package reachable from the workspace members."""
    found = {}
    for plat in entry.get("platforms", [None]):
        cmd = ["cargo", "metadata", "--format-version", "1", "--locked", "--manifest-path",
               os.path.join(ROOT, entry["manifest"])]
        if plat:
            cmd += ["--filter-platform", plat]
        if offline:
            cmd.append("--offline")
        out = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")
        if out.returncode != 0:
            raise RuntimeError(f"cargo metadata failed for {entry['manifest']}:\n{out.stderr[-3000:]}")
        meta = json.loads(out.stdout)
        nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
        pkgs = {p["id"]: p for p in meta["packages"]}
        members = set(meta["workspace_members"])

        def reach(member_kinds):
            seen, stack = set(), list(members)
            while stack:
                i = stack.pop()
                if i in seen:
                    continue
                seen.add(i)
                for d in nodes[i]["deps"]:
                    kinds = {k["kind"] for k in d["dep_kinds"]}
                    wanted = member_kinds if i in members else {None, "build"}
                    if kinds & wanted:
                        stack.append(d["pkg"])
            return seen - members

        runtime = reach({None})
        build = reach({None, "build"}) - runtime
        dev = reach({None, "build", "dev"}) - runtime - build
        for use, ids in (("runtime", runtime), ("build", build), ("dev", dev)):
            for i in ids:
                p = pkgs[i]
                key = (p["name"], p["version"])
                lic = p.get("license") or ("see " + p["license_file"] if p.get("license_file") else "")
                rank = {"runtime": 0, "build": 1, "dev": 2}
                if key not in found or rank[use] < rank[found[key][3]]:
                    found[key] = (p["name"], p["version"], lic, use)
    return sorted(found.values(), key=lambda r: (r[0].lower(), r[1]))


def npm_packages(entry):
    with open(os.path.join(ROOT, entry["lockfile"]), encoding="utf-8") as f:
        lock = json.load(f)
    found = {}
    for path, p in lock.get("packages", {}).items():
        if not path:
            continue
        name = p.get("name") or path.rsplit("node_modules/", 1)[-1]
        use = "dev" if p.get("dev") else "runtime"
        if p.get("optional"):
            use += ", optional"
        found[(name, p.get("version", ""))] = (name, p.get("version", ""), p.get("license", ""), use)
    return sorted(found.values(), key=lambda r: (r[0].lower(), r[1]))


def render(cfg, sections, problems):
    L = [f"# {cfg['title']}", "", "Generated by `scripts/third-party-inventory.py` from the lock files; do not edit by hand. "
         "Run it again after a dependency change (CI checks that this file is up to date).", ""]
    L += cfg.get("intro", [])
    if cfg.get("intro"):
        L.append("")
    L += ["Allowed licenses: " + ", ".join(f"`{a}`" for a in cfg["allowed"]) + ". Packages outside them: "
          + ("none." if not problems else f"{len(problems)}, listed below."), ""]
    for label, rows, note in sections:
        uses = {}
        for r in rows:
            uses[r[3].split(",")[0]] = uses.get(r[3].split(",")[0], 0) + 1
        L += [f"## {label}", "", note, "", "Packages: " + ", ".join(f"{n} {u}" for u, n in sorted(uses.items())) + ".", "",
              "| Package | Version | License | Use |", "|---|---|---|---|"]
        for name, ver, lic, use in rows:
            L.append(f"| `{name}` | {ver} | {lic or '(none declared)'} | {use} |")
        L.append("")
    if problems:
        L += ["## To review", ""] + [f"- `{n}` {v}: {l or '(no license declared)'} ({s})" for s, n, v, l in problems] + [""]
    return "\n".join(L)


def main(argv):
    with open(os.path.join(HERE, "third-party.json"), encoding="utf-8") as f:
        cfg = json.load(f)
    allowed = set(cfg["allowed"])
    offline = "--offline" in argv
    sections, problems = [], []
    for e in cfg.get("cargo", []):
        rows = cargo_packages(e, offline)
        sections.append((e["label"], rows, e.get("note", "")))
    for e in cfg.get("npm", []):
        rows = npm_packages(e)
        sections.append((e["label"], rows, e.get("note", "")))
    for label, rows, _ in sections:
        for name, ver, lic, _use in rows:
            if not lic or not spdx_ok(lic, allowed):
                problems.append((label, name, ver, lic))
    text = render(cfg, sections, problems) + "\n"
    out = os.path.join(ROOT, cfg["output"])
    if "--check" in argv:
        try:
            current = open(out, encoding="utf-8").read()
        except OSError:
            current = ""
        stale = current.replace("\r\n", "\n") != text
        if stale:
            print(f"third-party-inventory: {cfg['output']} is out of date: run python scripts/third-party-inventory.py")
        for label, name, ver, lic in problems:
            print(f"third-party-inventory: {label}: {name} {ver}: license not allowed or not declared: {lic!r}")
        return 1 if stale or problems else 0
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print(f"third-party-inventory: wrote {cfg['output']} ({sum(len(r) for _, r, _ in sections)} packages, "
          f"{len(problems)} to review)")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Exception as e:  # noqa: BLE001 - any failure of the tool itself is exit code 2
        print(f"third-party-inventory: could not run: {e}", file=sys.stderr)
        sys.exit(2)
