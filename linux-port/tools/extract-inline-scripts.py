#!/usr/bin/env python3
"""Extracts the JavaScript that macOS keeps inline in Swift string literals into shared/host-scripts/.

    python3 -I linux-port/tools/extract-inline-scripts.py [--check]

--check: exit 1 if a shared file differs from the Swift source (drift), without writing.
Swift multi-line literal rules applied: the closing delimiter's indentation is removed from every line, escapes
\\\\ \\" \\' \\n \\t \\r \\0 \\u{…} are decoded, a trailing backslash joins lines, and interpolation \\( is refused.
"""
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent.parent
SWIFT = REPO / "Sources/Olaz/ZaloRuntime/OfficialClientHost.swift"
SHARED = REPO / "linux-port/shared/host-scripts"

# (Swift declaration, output file, header note)
TARGETS = [
    ("static let script", "console-hook.js", "ConsoleHandler.script"),
    ("static let workerPreludeBase", "dal-worker-prelude.js",
     "AsarSchemeHandler.workerPreludeBase; /*DEBUG_EVAL*/ is replaced by dal-worker-debug-eval.js in debug builds only"),
    ("static let workerDebugEval", "dal-worker-debug-eval.js", "AsarSchemeHandler.workerDebugEval, DEBUG builds only"),
]


def literal(source: str, decl: str) -> str:
    start = source.find(decl + ' = """\n')
    if start < 0 or source.find(decl + ' = """\n', start + 1) >= 0:
        sys.exit(f"extract: `{decl}` not found exactly once in {SWIFT.name}")
    body_start = source.index("\n", start) + 1
    close = re.compile(r'^([ \t]*)"""', re.M).search(source, body_start)
    if not close:
        sys.exit(f"extract: no closing delimiter for {decl}")
    indent = close.group(1)
    raw_lines = source[body_start:close.start()].split("\n")[:-1]
    lines = []
    for line in raw_lines:
        if line.strip() and not line.startswith(indent):
            sys.exit(f"extract: {decl}: line not indented like the closing delimiter: {line!r}")
        lines.append(line[len(indent):] if line.startswith(indent) else "")
    text = "\n".join(lines)
    if "\\(" in text:
        sys.exit(f"extract: {decl} uses string interpolation; extend the extractor")
    text = re.sub(r"\\\n", "", text)  # line continuation

    def unescape(m: re.Match) -> str:
        esc = m.group(1)
        if esc.startswith("u{"):
            return chr(int(esc[2:-1], 16))
        return {"\\": "\\", '"': '"', "'": "'", "n": "\n", "t": "\t", "r": "\r", "0": "\0"}[esc]

    return re.sub(r"\\(u\{[0-9a-fA-F]+\}|[\\\"'ntr0])", unescape, text)


def main() -> None:
    check = "--check" in sys.argv
    source = SWIFT.read_text()
    drift = []
    for decl, name, note in TARGETS:
        js = literal(source, decl)
        content = (f"// EXTRACTED VERBATIM from Sources/Olaz/ZaloRuntime/OfficialClientHost.swift ({note}) — "
                   f"do not edit here; re-run tools/sync-from-macos.sh\n{js}\n")
        path = SHARED / name
        if path.exists() and path.read_text() == content:
            continue
        drift.append(name)
        if not check:
            path.write_text(content)
    if drift:
        print(("DRIFT: " if check else "updated: ") + ", ".join(drift))
        sys.exit(1 if check else 0)
    print("inline scripts up to date")


if __name__ == "__main__":
    main()
