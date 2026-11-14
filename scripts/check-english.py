#!/usr/bin/env python3
"""English-only check: reports text that is not in English, outside the files allowed to hold other languages.

    python scripts/check-english.py              every file git tracks
    python scripts/check-english.py FILE...      only these files
    python scripts/check-english.py --explain    also print the exceptions that were used

Exit code 0: no finding. 1: findings, one per line as `path:line: reason: text`. 2: the check could not run.

A line is reported when it holds
  - a letter outside ASCII (accented Latin, Cyrillic, CJK, ...) in a word that is not on the allowed list, or
  - French: two different French function words, or one elided form (l', d', qu', ...) and one function word.
Punctuation and symbols outside ASCII (dashes, typographic quotes, arrows, math signs, Greek letters used as math
symbols) are fine. The string value of a locale key (`fr = "..."`, `"es": "..."`, `ja: '...'`) is a translation and is
not checked.

The exceptions, each with its reason, are in english-only.json next to this script:
  skip     files that are not checked at all (translations, vendored third-party code, test data in other languages)
  strings  files whose string literals are not checked, only their comments and code (user-facing text kept in two
           languages inside the code)
  comments files of which only the comment lines are checked (where strings nest, as in shell scripts)
  words   words with letters outside ASCII that are fine anywhere (language names, proper names)
  lines    single lines, by file and an exact substring (or a list of them)
Options: --root DIR checks another checkout, --config FILE uses other exceptions.
Standard library only, Python 3.8 or newer.
"""
import fnmatch
import json
import os
import re
import subprocess
import sys
import unicodedata

HERE = os.path.dirname(os.path.abspath(__file__))
CONFIG = os.path.join(HERE, "english-only.json")

# Words that are French and practically never English or code. Kept short on purpose: two of them on one line are
# needed (or one with an elided form), so a lone "des" or "est" in English text does not trip the check.
FRENCH_WORDS = set("""
le les des du une est sont pas avec dans qui que quand cette ces cet mais sans tout tous toute toutes
très aussi alors donc leur leurs nous vous notre votre nos vos ils elles elle sinon puis encore chaque comme peut
doit fait faire ici jeu jeux joueur joueurs fichier fichiers dossier dossiers valeur valeurs erreur lanceur partie
parties serveur serveurs réglage réglages être déjà après avant depuis où même lorsque selon entre
""".split())
ELISION = re.compile(r"(?<![\w'])(?:l|d|qu|n|c|s|j|jusqu|lorsqu|puisqu)['’](?=[a-zàâäçéèêëîïôöùûüÿœ])", re.I)
WORD = re.compile(r"[^\W\d_]+(?:['’][^\W\d_]+)?", re.U)
LOCALES = r"(?:fr|es|ja|ru|de|it|ko|zh|pt|pt-BR|pt_BR|pt-br|zh-Hans|zh_Hans|zh-hans|zh-Hant|en-GB)"
LOCALE_VALUE = re.compile(r"""(?<![\w-])(["']?)""" + LOCALES + r"""\1\s*[:=]\s*("(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')""")
COMMENT_LINE = re.compile(r"^\s*(?:#|//|/\*|\*|;|--|<!--)")
STRING = re.compile(r'''"(?:[^"\\]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`''', re.S)


def glob_match(path, pattern):
    """fnmatch on '/'-separated paths where '**' crosses folders and '*' does not."""
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


def any_glob(path, patterns):
    return any(glob_match(path, p) for p in patterns)


def is_foreign_letter(ch):
    if ord(ch) < 128 or not unicodedata.category(ch).startswith("L"):
        return False
    if 0x0370 <= ord(ch) <= 0x03FF:      # Greek letters: math symbols in comments (sigma, mu, phi...)
        return False
    if ch in "µªº":                      # micro sign, ordinal indicators
        return False
    return True


def blank_strings(text, rx):
    """Replaces the inside of every string literal by spaces, keeping line breaks (so line numbers stay right)."""
    def repl(m):
        s = m.group(0)
        return s[0] + re.sub(r"[^\n]", " ", s[1:-1]) + s[-1]
    return rx.sub(repl, text)


def blank_locale_values(line):
    def repl(m):
        whole, value = m.group(0), m.group(2)
        return whole[: len(whole) - len(value)] + value[0] + " " * (len(value) - 2) + value[-1]
    return LOCALE_VALUE.sub(repl, line)


def check_line(line, allowed_words):
    """Returns the reason the line is not English, or None."""
    for w in WORD.findall(line):
        if any(is_foreign_letter(c) for c in w) and w not in allowed_words and w.strip("'’") not in allowed_words:
            return f"non-English word '{w}'"
    words = {w.lower() for w in WORD.findall(line)}
    fr = words & FRENCH_WORDS
    el = ELISION.findall(line)
    if len(fr) >= 2 or (fr and el) or len(el) >= 2:
        return "French words (" + ", ".join(sorted(fr) + [e + "'" for e in el])[:80] + ")"
    return None


def tracked_files(root):
    out = subprocess.run(["git", "-C", root, "ls-files", "-z"], capture_output=True)
    if out.returncode != 0:
        raise RuntimeError("git ls-files failed: " + out.stderr.decode(errors="replace"))
    return [p for p in out.stdout.decode("utf-8").split("\0") if p]


def main(argv):
    explain = "--explain" in argv
    args = [a for a in argv if a != "--explain"]
    config, root = CONFIG, None
    while args and args[0] in ("--root", "--config"):
        if args[0] == "--root":
            root = args[1]
        else:
            config = args[1]
        args = args[2:]
    if root is None:
        root = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, cwd=HERE).stdout.strip()
    with open(config, encoding="utf-8") as f:
        cfg = json.load(f)
    skip = [p for e in cfg.get("skip", []) for p in e["paths"]]
    strings = [p for e in cfg.get("strings", []) for p in e["paths"]]
    comments = [p for e in cfg.get("comments", []) for p in e["paths"]]
    allowed_words = {w for e in cfg.get("words", []) for w in e["words"]}
    line_rules = cfg.get("lines", [])
    files = [os.path.relpath(os.path.abspath(a), root).replace("\\", "/") for a in args] if args else tracked_files(root)
    findings, used = [], set()
    for rel in files:
        if any_glob(rel, skip):
            used.add(("skip", rel))
            continue
        try:
            with open(os.path.join(root, rel), "rb") as f:
                data = f.read()
        except OSError:
            continue
        if b"\0" in data[:8192]:
            continue
        text = data.decode("utf-8", errors="replace")
        if any_glob(rel, strings):
            text = blank_strings(text, STRING)
            used.add(("strings", rel))
        if any_glob(rel, comments):
            text = "\n".join(line if COMMENT_LINE.match(line) else "" for line in text.split("\n"))
            used.add(("comments", rel))
        for n, line in enumerate(text.splitlines(), 1):
            reason = check_line(blank_locale_values(line), allowed_words)
            if not reason:
                continue
            rule = next((r for r in line_rules if glob_match(rel, r["path"])
                         and any(c in line for c in ([r["contains"]] if isinstance(r["contains"], str) else r["contains"]))),
                        None)
            if rule:
                used.add(("line", f"{rel}:{n}"))
                continue
            findings.append(f"{rel}:{n}: {reason}: {line.strip()[:140]}")
    for f in findings:
        print(f)
    if explain:
        for kind, what in sorted(used):
            print(f"allowed ({kind}): {what}")
    print(f"check-english: {len(findings)} finding(s) in {len(files)} file(s)", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Exception as e:  # noqa: BLE001 - any failure of the check itself is exit code 2
        print(f"check-english: could not run: {e}", file=sys.stderr)
        sys.exit(2)
