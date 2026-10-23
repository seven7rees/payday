#!/usr/bin/env bash
# Recreate ref/ (third-party sources the spec cites). Safe to re-run. Needs git, curl, python3.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p ref/site ref/briefs
[ -d ref/bordrless-programs/.git ] || git clone https://github.com/BordrlessDex/bordrless-programs ref/bordrless-programs
git -C ref/bordrless-programs checkout c6676977ddc71357c15a2af7d68e04bb095e23a2
[ -d ref/bordrless-sdk/.git ] || git clone https://github.com/BordrlessDex/bordrless-sdk ref/bordrless-sdk
git -C ref/bordrless-sdk checkout d1a4093f824cf60718532108039d8d49d9791267
# The three Workshop briefs sit in the page's RSC payload, not behind clicks.
curl -sS -A "Mozilla/5.0" https://bordrless.app/sdk/build -o ref/build.html
python3 -I tools/extract_briefs.py ref/build.html ref/briefs
for p in sdk/companions docs/companions sdk/hooks workshop/submit; do
  curl -sS -A "Mozilla/5.0" "https://bordrless.app/$p" -o "ref/site/$(echo "$p" | tr '/' '_').html"
done
echo "ref/ ready"
