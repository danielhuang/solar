#!/usr/bin/env bash
# Throughput harness: run each command 3x, print each run's wall-clock (s) and
# peak RSS (MB), and the median wall-clock. Usage: bench/run.sh runs the full
# matrix. Requires the Solar binaries (target/allocs3, target/threads_list2),
# the C binaries (bench/c/*), and the Java classes (bench/java/*.class) built.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUNS=3

# Run one command, return "wall_seconds peakRSS_KB". Samples /proc/PID/status
# VmHWM while the process runs (no /usr/bin/time on this box).
run_once() { # cmd...
    "$@" >/dev/null 2>&1 &
    local pid=$! hwm=0 v
    local start end
    start=$(date +%s.%N)
    while kill -0 "$pid" 2>/dev/null; do
        v=$(grep -m1 VmHWM /proc/$pid/status 2>/dev/null | awk '{print $2}')
        [ -n "$v" ] && [ "$v" -gt "$hwm" ] 2>/dev/null && hwm=$v
        sleep 0.02
    done
    wait "$pid"
    end=$(date +%s.%N)
    echo "$(awk "BEGIN{printf \"%.2f\", $end-$start}") $hwm"
}

measure() { # label cmd...
    local label="$1"; shift
    local times=() rss=() out e m
    for ((i = 0; i < RUNS; i++)); do
        out=$(run_once "$@"); read -r e m <<<"$out"
        times+=("$e"); rss+=("$m")
    done
    local med maxrss
    med=$(printf '%s\n' "${times[@]}" | sort -n | sed -n '2p')
    maxrss=$(printf '%s\n' "${rss[@]}" | sort -n | tail -1)
    printf '%-26s median=%6ss  runs=[%s]  peakRSS=%sMB\n' \
        "$label" "$med" "$(IFS=,; echo "${times[*]}")" "$((maxrss / 1024))"
}

echo "### load before run:"; uptime
JAVA_OPTS="-Xmx8g"
cd "$ROOT/bench/java" || exit 1

for bench in allocs3:Allocs3 threads:ThreadsList2; do
    solar_bin="${bench%%:*}"; class="${bench##*:}"
    [ "$solar_bin" = threads ] && solar_bin=threads_list2
    echo; echo "==== $class ===="
    measure "Solar"            "$ROOT/target/$solar_bin"
    measure "C (malloc/free)"  "$ROOT/bench/c/$solar_bin"
    measure "Go"               "$ROOT/bench/go/$solar_bin"
    measure "Java G1"          java $JAVA_OPTS -XX:+UseG1GC "$class"
    measure "Java Parallel"    java $JAVA_OPTS -XX:+UseParallelGC "$class"
    measure "Java ZGC gen"     java $JAVA_OPTS -XX:+UseZGC -XX:+ZGenerational "$class"
    measure "Java ZGC non-gen" java $JAVA_OPTS -XX:+UseZGC "$class"
    measure "Java Shenandoah"  java $JAVA_OPTS -XX:+UseShenandoahGC "$class"
done
echo; echo "### load after run:"; uptime
