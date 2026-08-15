#!/usr/bin/env bash
set -euo pipefail

output=${1:-fuzz/seeds/elf_convert}
source_file=${2:-fuzz/fixtures/seed.c}
compiler=${CC:-cc}
split_compiler=${SPLIT_CC:-$compiler}
objcopy=${OBJCOPY:-objcopy}
llvm_dwp=${LLVM_DWP:-}

write_envelope() {
  local destination=$1 control=$2
  shift 2
  python3 - "$destination" "$control" "$@" <<'PY'
import pathlib
import struct
import sys

destination = pathlib.Path(sys.argv[1])
control = int(sys.argv[2])
parts = [pathlib.Path(path).read_bytes() if path else b"" for path in sys.argv[3:]]
if len(parts) != 5:
    raise SystemExit("an ELF fuzz envelope requires exactly five payload paths")
destination.write_bytes(
    b"GSYMELF\0" + bytes([control]) + struct.pack("<5I", *(len(part) for part in parts)) + b"".join(parts)
)
PY
}

if [[ -z "$llvm_dwp" ]]; then
  for candidate in /usr/lib/llvm-*/bin/llvm-dwp llvm-dwp; do
    if [[ -x "$candidate" ]] || command -v "$candidate" >/dev/null 2>&1; then
      llvm_dwp=$candidate
    fi
  done
fi

mkdir -p "$output"
temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT

common=(-O2 -fno-omit-frame-pointer -fdebug-prefix-map="$PWD"=/src)
for version in 2 3 4 5; do
  "$compiler" "${common[@]}" "-gdwarf-$version" "$source_file" \
    -Wl,--build-id=none -o "$output/dwarf-$version.elf"
done

"$compiler" "${common[@]}" -gdwarf-5 -c "$source_file" \
  -o "$output/relocatable-dwarf-5.o"

cp "$output/dwarf-5.elf" "$output/compressed-dwarf-5.elf"
"$objcopy" --compress-debug-sections=zlib "$output/compressed-dwarf-5.elf"

cp "$output/dwarf-5.elf" "$temporary/separate-image.elf"
"$objcopy" --only-keep-debug "$temporary/separate-image.elf" \
  "$temporary/separate-debug.elf"
"$objcopy" --strip-debug "$temporary/separate-image.elf"
write_envelope "$output/separate-debug.bundle" 0 \
  "$temporary/separate-image.elf" "$temporary/separate-debug.elf" '' '' ''

"$split_compiler" "${common[@]}" -gdwarf-4 -gsplit-dwarf -c "$source_file" \
  -o "$temporary/split.o"
"$split_compiler" "$temporary/split.o" -Wl,--build-id=none -o "$temporary/split.elf"
split_dwo=$temporary/split.dwo
if [[ ! -f "$split_dwo" ]]; then
  split_dwo=$(find "$temporary" -maxdepth 1 -name '*.dwo' -print -quit)
fi
cp "$split_dwo" "$output/split-dwarf.dwo"

if [[ -n "$llvm_dwp" ]]; then
  timeout 30 "$llvm_dwp" "$split_dwo" -o "$temporary/split.dwp"
  write_envelope "$output/split-dwarf-dwp.bundle" 0 \
    "$temporary/split.elf" '' '' '' "$temporary/split.dwp"
else
  echo 'warning: llvm-dwp not found; DWP seed was not generated' >&2
fi
