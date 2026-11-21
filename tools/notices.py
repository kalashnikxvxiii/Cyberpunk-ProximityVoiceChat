#!/usr/bin/env python3
"""Writes THIRD-PARTY-NOTICES.txt for what ships in a binary: every crate
linked into the given packages (normal dependencies only, for the given
target), with its license text, plus the C/C++ libraries they bundle.

usage: notices.py TARGET PACKAGE... > THIRD-PARTY-NOTICES.txt
"""
import glob, json, os, subprocess, sys

target, roots = sys.argv[1], sys.argv[2:]
meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--filter-platform", target]))
pkgs = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
workspace = set(meta["workspace_members"])

seen, stack = set(), [i for i, p in pkgs.items() if p["name"] in roots and i in workspace]
while stack:
    i = stack.pop()
    if i in seen:
        continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        if any(k["kind"] is None for k in d["dep_kinds"]):
            stack.append(d["pkg"])

def texts(directory):
    out = []
    for pattern in ("LICENSE*", "LICENCE*", "COPYING*", "NOTICE*", "COPYRIGHT*", "license*", "LICENSES/*"):
        for f in sorted(glob.glob(os.path.join(directory, pattern))):
            if os.path.isfile(f) and not any(f == o[2] for o in out):
                out.append((os.path.relpath(f, directory), open(f, encoding="utf-8", errors="replace").read().strip(), f))
    return [(n, b) for n, b, _ in out]

MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE."""

print("VoiceChat - third-party software\n" + "=" * 34)
print("\nVoiceChat includes the following open-source software. Their licenses")
print("and copyright notices follow.\n")
bundled = []
# Identical license texts are printed once, at the end, and referenced.
bodies = {}

def ref(name, body):
    key = body.strip()
    if key not in bodies:
        bodies[key] = (f"T{len(bodies) + 1}", name)
    return bodies[key][0]

for i in sorted(seen - workspace, key=lambda i: (pkgs[i]["name"], pkgs[i]["version"])):
    p = pkgs[i]
    d = os.path.dirname(p["manifest_path"])
    print("-" * 72)
    print(f"{p['name']} {p['version']}  ({p.get('license') or 'see below'})")
    if p.get("repository"):
        print(p["repository"])
    if p.get("authors"):
        print("Copyright: " + ", ".join(p["authors"]))
    found = texts(d)
    for name, body in found:
        print(f"  {name}: see text {ref(name, body)}")
    if not found and "MIT" in (p.get("license") or ""):
        # The crate doesn't ship a license file; the MIT terms it declares:
        print("  MIT License (no license file shipped): see text " + ref("MIT", MIT))
    # C/C++ sources vendored inside crates.
    for sub in ("opus", "deps/RED4ext.SDK"):
        sd = os.path.join(d, sub)
        if os.path.isdir(sd) and texts(sd):
            bundled.append((f"{p['name']}/{sub}", texts(sd)))
for name, ts in bundled:
    print("-" * 72)
    print(f"{name} (bundled C/C++ library)")
    for fname, body in ts:
        print(f"  {fname}: see text {ref(fname, body)}")
print("\n" + "=" * 72 + "\nLICENSE TEXTS\n" + "=" * 72)
for body, (tid, name) in bodies.items():
    print(f"\n--- {tid} ({name}) ---\n{body}\n")
