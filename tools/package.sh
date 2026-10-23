#!/bin/sh
# Copy the Payday pair from the moon-wick build checkout into this repo and lay out submission/
# exactly as the Workshop submit page takes it. No keypairs, no build output.
set -eu
SRC="${1:-$HOME/Desktop/claude/moon-wick}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$HERE"
rm -rf hook_variants script_variants submission/hook submission/script tests
mkdir -p hook_variants/payday script_variants/payday submission/hook submission/script tests/harness tests/litesvm
for d in hook_variants/payday script_variants/payday; do
  cp -R "$SRC/$d/src" "$d/src"
  cp "$SRC/$d/Cargo.toml" "$d/"
done
cp "$SRC/hook_variants/payday/hook.json" hook_variants/payday/
cp "$SRC/script_variants/payday/keeper.json" script_variants/payday/
cp -R "$SRC/hook_variants/payday/src" submission/hook/src
cp "$SRC/hook_variants/payday/hook.json" submission/hook/
cp -R "$SRC/script_variants/payday/src" submission/script/src
cp "$SRC/script_variants/payday/keeper.json" submission/script/
cp "$SRC/variants/payday/notes.txt" submission/notes.txt
cp "$SRC"/harness/tests/t2[3456]_payday_*.rs tests/harness/
cp "$SRC/litesvm/tests/t11_payday.rs" "$SRC/litesvm/tests/t12_payday_tiers.rs" "$SRC/litesvm/tests/t13_payday_hostile.rs" tests/litesvm/
cp "$SRC/WALLET.txt" WALLET.txt
if find . -name "*keypair*" -not -path "./.git/*" | grep -q .; then echo "keypair found, stop"; exit 1; fi
echo "files:"; find submission -type f | sort
echo "hook: $(ls submission/hook/src | wc -l | tr -d ' ') files, $(cat submission/hook/src/*.rs | wc -c | tr -d ' ') bytes"
echo "script: $(ls submission/script/src | wc -l | tr -d ' ') files, $(cat submission/script/src/*.rs | wc -c | tr -d ' ') bytes"
printf "notes chars: "; python3 -I -c "print(len(open('submission/notes.txt').read().strip()))"
