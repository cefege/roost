#!/bin/bash
# mut.sh <crate> <test-binary> <test-name> <file> <old> <new>: apply one mutation, run the
# named test, report KILLED (test failed) or SURVIVED, always restore the file.
crate=$1 bin=$2 name=$3 file=/home/mike/repos/roost-v3-worker/$4 old=$5 new=$6
cp "$file" "$file.mutbak"
python3 - "$file" "$old" "$new" <<'PY'
import sys
p, old, new = sys.argv[1:4]
s = open(p).read()
if old not in s:
    print("ANCHOR MISSING", p); sys.exit(3)
open(p, "w").write(s.replace(old, new, 1))
PY
rc=$?
if [ $rc -eq 0 ]; then
  out=$(/home/mike/wl/wcargo.sh test -p "$crate" --test "$bin" -- "$name" 2>&1)
  if echo "$out" | grep -qE "^test .*$name.* FAILED|test result: FAILED"; then echo "KILLED $bin::$name ($4)";
  elif echo "$out" | grep -q "^error"; then echo "BUILD-ERROR $bin::$name ($4)"; echo "$out" | grep -A5 "^error" | head -12;
  else echo "SURVIVED $bin::$name ($4)"; echo "$out" | grep -E "^test result"; fi
fi
mv "$file.mutbak" "$file"
