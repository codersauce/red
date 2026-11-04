#!/bin/sh
# Five paired repetitions. Run from the repository root on an idle machine.
set -eu
if [ "$#" -ne 4 ]; then
    echo "usage: sh scripts/felix/input-matrix.sh DEV_BINARY RELEASE_BINARY OUTPUT_DIR LABEL" >&2
    exit 2
fi
dev=$1 release=$2 out=$3 label=$4
mkdir -p "$out"
for run in 1 2 3 4 5; do
    if [ $((run % 2)) = 1 ]; then models='helix vim'; else models='vim helix'; fi
    for profile in release dev; do
        if [ "$profile" = release ]; then binary=$release; else binary=$dev; fi
        for scenario in typing horizontal vertical; do
            for model in $models; do
                file="$out/$label-$profile-$scenario-$model-$run.json"
                python3 scripts/felix/input_bench.py --binary "$binary" \
                    --scenario "$scenario" --model "$model" > "$file.tmp"
                mv "$file.tmp" "$file"
            done
        done
    done
done
python3 scripts/felix/input_limits.py "$out" "$label"
