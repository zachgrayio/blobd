#!/usr/bin/env bash

set -Eeuo pipefail

if [[ "${1:-}" == "--inside" ]]; then
  suite=${2:-quick}
  cd /workspace
  if [[ "$suite" == "--suite" ]]; then
    run_dir="benchmark-runner/cfg/clock/runs/$(date -u +%Y%m%dT%H%M%SZ)-$$"
    mkdir -p "$run_dir"
  fi
  truncate -s 32M /tmp/blobd-clock.img
  loop_device=$(losetup --find --show /tmp/blobd-clock.img)
  trap 'losetup -d "$loop_device"' EXIT
  ln -s "$loop_device" /tmp/blobd-clock-device

  cargo build --release -p benchmark-runner -p blobd-stochastic-stresser -p benchmark-plotter
  BLOBD_DESTRUCTIVE_TEST_DEVICE=/tmp/blobd-clock-device cargo test --release -p libblobd-direct --lib

  RUST_LOG=info "$CARGO_TARGET_DIR/release/blobd-stochastic-stresser" stochastic-stresser/cfg/clock.yaml 2>&1 |
    tee benchmark-runner/cfg/clock/stresser.baseline.log
  RUST_LOG=info "$CARGO_TARGET_DIR/release/blobd-stochastic-stresser" stochastic-stresser/cfg/clock.yaml --cache-eviction 2>&1 |
    tee benchmark-runner/cfg/clock/stresser.clock.log
  if [[ "$suite" == "--suite" ]]; then
    cp benchmark-runner/cfg/clock/stresser.{baseline,clock}.log "$run_dir/"
  fi

  cd benchmark-runner
  benchmark="$CARGO_TARGET_DIR/release/benchmark-runner"
  if [[ "$suite" == "--suite" ]]; then
    run_dir="cfg/clock/runs/$(basename "$run_dir")"
    summary="$run_dir/samples.jsonl"

    run_case() {
      local profile=$1 repetition=$2 mode=$3 objects=$4 size=$5 concurrency=$6 partition_len=$7
      local result="cfg/clock/results.${mode}.${objects}o.${size}b.json"
      local log="$run_dir/${profile}.${repetition}.${mode}.log"
      local -a args=(clock --device /tmp/blobd-clock-device --partition-len "$partition_len" --skip-cache-drop --objects "$objects" --object-size "$size" --concurrency "$concurrency")
      if [[ "$mode" != baseline ]]; then
        args+=(--cache-eviction)
      fi
      if [[ "$mode" == pressure ]]; then
        args+=(--cache-pressure)
      fi
      printf '%s repetition %s %s\n' "$profile" "$repetition" "$mode"
      if ! "$benchmark" "${args[@]}" >"$log" 2>&1; then
        tail -80 "$log"
        exit 1
      fi
      cp "$result" "$run_dir/${profile}.${repetition}.${mode}.json"
      jq -cn --arg profile "$profile" --argjson repetition "$repetition" --arg mode "$mode" --slurpfile result "$result" \
        '{profile: $profile, repetition: $repetition, mode: $mode, result: $result[0]}' >>"$summary"
    }

    for profile in small-serial:2000:4096:1 medium-serial:400:16384:1 large-serial:100:65536:1 small-parallel:2000:4096:8 medium-parallel:400:16384:8 large-parallel:100:65536:8; do
      IFS=: read -r name objects size concurrency <<<"$profile"
      for repetition in 1 2 3; do
        if (( repetition % 2 )); then
          modes=(baseline clock)
        else
          modes=(clock baseline)
        fi
        for mode in "${modes[@]}"; do
          run_case "$name" "$repetition" "$mode" "$objects" "$size" "$concurrency" 16777216
        done
      done
    done
    for profile in pressure-2m:250:16384:2097152 pressure-4m:500:16384:4194304; do
      IFS=: read -r name objects size partition_len <<<"$profile"
      for repetition in 1 2 3; do
        run_case "$name" "$repetition" pressure "$objects" "$size" 1 "$partition_len"
      done
    done

    jq -s -r '
      def median(field): [.[] | .result | field | select(. != null)] | sort | if length == 0 then "-" else .[length / 2 | floor] end;
      (["profile", "mode", "runs", "create_s", "write_s", "commit_s", "read_s", "read_p95_ms", "evictions"] | @tsv),
      (group_by([.profile, .mode])[] |
        [.[0].profile, .[0].mode, length, median(.op.create.exec_secs), median(.op.write.exec_secs), median(.op.commit.exec_secs), median(.op.read.exec_secs), median(.op.read.latency.p95_ms), median(.cache_pressure.evictions)] | @tsv)
    ' "$summary" | tee "$run_dir/medians.tsv"
    "$CARGO_TARGET_DIR/release/benchmark-plotter" --suite "$summary"
    printf 'Saved all results to %s\n' "$run_dir"
  else
    "$benchmark" clock --device /tmp/blobd-clock-device --skip-cache-drop
    "$benchmark" clock --device /tmp/blobd-clock-device --skip-cache-drop --cache-eviction
    "$benchmark" clock --device /tmp/blobd-clock-device --partition-len 2097152 --skip-cache-drop --cache-eviction --cache-pressure --objects 250
    jq -r '[input_filename, (.op.create.exec_secs // "-"), (.op.read.exec_secs // "-"), (.cache_pressure.evictions // "-")] | @tsv' cfg/clock/results.{baseline.400o,clock.400o,pressure.250o}.16384b.json
  fi
  exit
fi

if [[ $# -gt 1 || ( $# -eq 1 && "$1" != "--suite" ) ]]; then
  echo "usage: bash benchmark-runner/docker-compare.sh [--suite]" >&2
  exit 2
fi

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
image=blobd-clock-compare:local
docker build -f "$root/benchmark-runner/Dockerfile.compare" -t "$image" "$root"
docker run --rm --privileged \
  --mount "type=bind,source=$root,target=/workspace" \
  --mount type=volume,source=blobd-clock-cargo,target=/usr/local/cargo/registry \
  --mount type=volume,source=blobd-clock-target,target=/target \
  -e CARGO_TARGET_DIR=/target -e CARGO_BUILD_JOBS=4 \
  -w /workspace "$image" bash benchmark-runner/docker-compare.sh --inside "${1:-quick}"