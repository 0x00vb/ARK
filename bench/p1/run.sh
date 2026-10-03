#!/bin/sh
# P1: live runs, 3 repetitions each. Usage: run.sh <provider:model> <name>:<harness>:<env> ...
# A config's env is space-free KEY=VALUE pairs joined by '+', or '-' for none.
set -u
m=$1; shift
for cfg in "$@"; do
  name=${cfg%%:*}; rest=${cfg#*:}; h=${rest%%:*}; envs=${rest#*:}
  set --
  [ "$envs" != "-" ] && for kv in $(echo "$envs" | tr '+' ' '); do set -- "$@" "$kv"; done
  env "$@" ./.ark-bench run --model "$m" --harness "$h" --reps 3 --turn-timeout 300 \
    --label "$name" --record "$name.jsonl" --traces "traces/$name" --out "$name.json" \
    > "$name.log" 2>&1
  echo "$name exit $?" >> done.txt
done
