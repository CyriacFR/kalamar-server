#!/usr/bin/env python3
"""Relative link check of the Markdown files: every link to a file of the repository points to a file or folder that
exists, and every #anchor to a Markdown heading that exists (GitHub's heading ids).

    python scripts/check-links.py                         every Markdown file git tracks
    python scripts/check-links.py --skip 'vendor/**' ...  minus these paths (globs, ** crosses folders)

Exit code 0: no broken link. 1: broken links, one per line as `path:line: target: reason`. 2: the check could not run.
External links (https:, mailto:, ...) are not fetched: this check needs no network.
Standard library only, Python 3.8 or newer.
"""
import os
import re
import subprocess
import sys
import urllib.parse

INLINE = re.compile(r"!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+(?:\"[^\"]*\"|'[^']*'|\([^)]*\)))?\s*\)")
REFDEF = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)")
HTML = re.compile(r"""<(?:a|img)\b[^>]*?\b(?:href|src)\s*=\s*["']([^"']+)["']""", re.I)
ANCHOR_TAG = re.compile(r"""<a\b[^>]*?\b(?:id|name)\s*=\s*["']([^"']+)["']""", re.I)
HEADING = re.compile(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$")
SETEXT = re.compile(r"^\s{0,3}(=+|-+)\s*$")
SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")
# github-slugger: what it removes from a heading's text (ASCII punctuation but - and _, and common Unicode punctuation)
SLUG_DROP = re.compile(r"[\x00-\x1f!-,./:-@\[-^`{-~ -¿×÷ -⁯←-⯿　-〿]")


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
    return re.match(rx + r"\Z", path) is not None


def strip_code(lines, blank_spans=True):
    """Yields (line number, text) outside fenced code blocks, with inline code spans blanked (unless told not to)."""
    fence = None
    for n, line in enumerate(lines, 1):
        m = re.match(r"^\s{0,3}(`{3,}|~{3,})", line)
        if m:
            if fence is None:
                fence = m.group(1)[0] * len(m.group(1))
                continue
            if line.strip().startswith(fence):
                fence = None
                continue
        if fence is not None:
            continue
        if not blank_spans:
            yield n, line
            continue
        yield n, re.sub(r"(`+)(?:(?!\1).)+?\1", lambda m: " " * len(m.group(0)), line)


def slug(text):
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]+>", "", text).replace("`", "").replace("*", "")
    return SLUG_DROP.sub("", text.strip().lower()).replace(" ", "-")


_ANCHORS = {}


def anchors_of(path):
    if path in _ANCHORS:
        return _ANCHORS[path]
    seen, out = {}, set()
    try:
        lines = open(path, encoding="utf-8", errors="replace").read().splitlines()
    except OSError:
        lines = []
    prev = None
    for n, line in strip_code(lines, blank_spans=False):
        m = HEADING.match(line)
        text = m.group(2) if m else None
        if text is None and prev and prev.strip() and SETEXT.match(line) and not prev.lstrip().startswith(("|", "-", "*")):
            text = prev.strip()
        if text is not None:
            s = slug(text)
            k = seen.get(s, 0)
            seen[s] = k + 1
            out.add(s if k == 0 else f"{s}-{k}")
        for a in ANCHOR_TAG.findall(line):
            out.add(a)
        prev = line
    _ANCHORS[path] = out
    return out


def main(argv):
    skip, args = [], list(argv)
    while args and args[0] == "--skip":
        skip.append(args[1])
        args = args[2:]
    root = args[0] if args else subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True,
                                               text=True).stdout.strip()
    files = subprocess.run(["git", "-C", root, "ls-files", "-z", "--", "*.md"], capture_output=True)
    if files.returncode != 0:
        raise RuntimeError(files.stderr.decode(errors="replace"))
    md = [p for p in files.stdout.decode("utf-8").split("\0") if p and not any(glob_match(p, s) for s in skip)]
    broken, count = [], 0
    for rel in md:
        full = os.path.join(root, rel)
        try:
            lines = open(full, encoding="utf-8", errors="replace").read().splitlines()
        except OSError:
            continue
        for n, line in strip_code(lines):
            targets = [m.group(1) for m in INLINE.finditer(line)] + HTML.findall(line)
            m = REFDEF.match(line)
            if m:
                targets.append(m.group(1))
            for t in targets:
                t = t.strip("<>")
                if not t or SCHEME.match(t) or t.startswith("//"):
                    continue
                count += 1
                path, _, frag = t.partition("#")
                path = urllib.parse.unquote(path.split("?")[0])
                if path.startswith("/"):
                    dest = os.path.normpath(os.path.join(root, path.lstrip("/")))
                elif path:
                    dest = os.path.normpath(os.path.join(os.path.dirname(full), path))
                else:
                    dest = full
                if not os.path.exists(dest):
                    broken.append(f"{rel}:{n}: {t}: no such file or folder")
                    continue
                if os.path.relpath(dest, root).startswith(".."):
                    broken.append(f"{rel}:{n}: {t}: outside the repository")
                    continue
                if frag and dest.lower().endswith(".md") and os.path.isfile(dest):
                    if urllib.parse.unquote(frag) not in anchors_of(dest):
                        broken.append(f"{rel}:{n}: {t}: no heading with the id #{frag}")
    for b in broken:
        print(b)
    print(f"check-links: {len(broken)} broken of {count} relative link(s) in {len(md)} Markdown file(s)", file=sys.stderr)
    return 1 if broken else 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Exception as e:  # noqa: BLE001 - any failure of the check itself is exit code 2
        print(f"check-links: could not run: {e}", file=sys.stderr)
        sys.exit(2)
