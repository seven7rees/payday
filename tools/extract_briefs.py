"""Extract the three Workshop brief prompts from a saved bordrless.app/sdk/build HTML file.

The prompts are Next.js RSC text rows ("<id>:T<hex byte length>,<text>").

usage: python3 -I extract_briefs.py <build.html> <out_dir>
"""
import json
import re
import sys
from pathlib import Path

html_path, out_dir = Path(sys.argv[1]), Path(sys.argv[2])
html = html_path.read_text(encoding="utf-8")

chunk_re = re.compile(r'self\.__next_f\.push\(\[1,"((?:[^"\\]|\\.)*)"\]\)')
stream = "".join(json.loads('"' + m.group(1) + '"') for m in chunk_re.finditer(html))

markers = {
    "hook": "You are helping me write a token hook for Bordrless",
    "companion": "You are helping me write a fee hook for Bordrless",
    "pair": "You are helping me write a pair for Bordrless",
}
header_re = re.compile(r"T([0-9a-f]+),$")
out_dir.mkdir(parents=True, exist_ok=True)
for name, marker in markers.items():
    idx = stream.find(marker)
    if idx < 0:
        print(f"{name}: marker not found")
        continue
    header = header_re.search(stream[max(0, idx - 16):idx])
    if not header:
        print(f"{name}: no text-row header before marker")
        continue
    nbytes = int(header.group(1), 16)
    text = stream[idx:].encode("utf-8")[:nbytes].decode("utf-8")
    (out_dir / f"{name}-brief.txt").write_text(text + "\n", encoding="utf-8")
    print(f"{name}: declared {nbytes} bytes, wrote {len(text)} chars, {text.count(chr(10)) + 1} lines")
