#!/usr/bin/env bash
# GC pause latency harness. Reports max and p50 STW stall (ms) per collector:
#   Solar = pause1 + pause2 per cycle (SOLAR_PRINT_GC_STATS)
#   Java  = "At safepoint" per safepoint (-Xlog:safepoint)
#   C     = none (no collector)
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# stats: reads ms values (one per line) on stdin, prints "max p50".
stats() {
    sort -n | awk '{a[NR]=$1} END{
        if (NR==0){print "—  —"; exit}
        printf "%.2f  %.2f", a[NR], a[int((NR+1)/2)]
    }'
}

solar() { # solar_bin  -> sum pause1+pause2 (ms) per cycle
    SOLAR_PRINT_GC_STATS=1 "$ROOT/target/$1" 2>&1 | awk '
        function ms(t,  v){ v=t+0; if(t~/µs/)return v/1000; if(t~/ms/)return v; return v*1000 }
        /pause1/ {
            match($0,/pause1 [0-9.]+(µs|ms|s)/); p1=substr($0,RSTART+7,RLENGTH-7);
            match($0,/pause2 [0-9.]+(µs|ms|s)/); p2=substr($0,RSTART+7,RLENGTH-7);
            print ms(p1)+ms(p2)
        }' | stats
}

go_pauses() { # go_bin  -> STW pauses (ms): sweep-term + mark-term per cycle
    GODEBUG=gctrace=1 "$ROOT/bench/go/$1" 2>&1 |
        grep -oE '[0-9.]+\+[0-9.]+\+[0-9.]+ ms clock' |
        awk '{split($1,a,"+"); print a[1]+a[3]}' | stats
}

java_pauses() { # class gcflags...
    local class="$1"; shift
    java -Xmx8g "$@" -Xlog:safepoint "$class" 2>&1 |
        grep -oE 'At safepoint: [0-9]+ ns' | awk '{print $3/1e6}' | stats
}

cd "$ROOT/bench/java" || exit 1
printf '%-22s %12s %12s\n' "collector" "max(ms)" "p50(ms)"
for b in allocs3:Allocs3 threads_list2:ThreadsList2; do
    sbin="${b%%:*}"; class="${b##*:}"
    echo "==== $class ===="
    printf '%-22s %s\n' "Solar"            "$(solar "$sbin")"
    printf '%-22s %s\n' "C (malloc/free)"  "none  none"
    printf '%-22s %s\n' "Go"               "$(go_pauses "$sbin")"
    printf '%-22s %s\n' "Java G1"          "$(java_pauses "$class" -XX:+UseG1GC)"
    printf '%-22s %s\n' "Java Parallel"    "$(java_pauses "$class" -XX:+UseParallelGC)"
    printf '%-22s %s\n' "Java ZGC gen"     "$(java_pauses "$class" -XX:+UseZGC -XX:+ZGenerational)"
    printf '%-22s %s\n' "Java ZGC non-gen" "$(java_pauses "$class" -XX:+UseZGC)"
    printf '%-22s %s\n' "Java Shenandoah"  "$(java_pauses "$class" -XX:+UseShenandoahGC)"
done
