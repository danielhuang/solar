// In-process STW-pause tracer for the .NET benchmarks, enabled by BENCH_GC_TRACE=1.
//
// The CLR has no built-in gctrace knob like Go's GODEBUG=gctrace=1 or Java's
// -Xlog:safepoint, so we subscribe in-process to the runtime's GC EventSource
// and time each stop-the-world window: from GCSuspendEEBegin (the EE — the
// managed execution engine — is suspended) to GCRestartEEEnd (mutators resume).
// That delta is the actual application stall, the same quantity bench.py reads
// from the other runtimes. Each pause is printed to stderr as "GCPAUSE <ms> ms"
// for the harness to parse. The listener is created only when the env var is set
// so it adds no overhead to plain throughput runs.

using System.Diagnostics.Tracing;

internal static class GcPause
{
    private static Listener? _listener;

    public static void MaybeStart()
    {
        if (Environment.GetEnvironmentVariable("BENCH_GC_TRACE") == "1")
            _listener = new Listener();
    }

    private sealed class Listener : EventListener
    {
        // GC keyword of the "Microsoft-Windows-DotNETRuntime" provider.
        private const EventKeywords GCKeyword = (EventKeywords)0x1;

        private DateTime _suspendStart;
        private bool _inSuspend;

        protected override void OnEventSourceCreated(EventSource source)
        {
            if (source.Name == "Microsoft-Windows-DotNETRuntime")
                EnableEvents(source, EventLevel.Informational, GCKeyword);
        }

        protected override void OnEventWritten(EventWrittenEventArgs e)
        {
            switch (e.EventName)
            {
                case "GCSuspendEEBegin_V1":
                case "GCSuspendEEBegin":
                    _suspendStart = e.TimeStamp;
                    _inSuspend = true;
                    break;
                case "GCRestartEEEnd_V1":
                case "GCRestartEEEnd":
                    if (_inSuspend)
                    {
                        double ms = (e.TimeStamp - _suspendStart).TotalMilliseconds;
                        Console.Error.WriteLine($"GCPAUSE {ms:F4} ms");
                        _inSuspend = false;
                    }
                    break;
            }
        }
    }
}
