using System.Diagnostics;

namespace OpenTaskbar.Services;

/// <summary>
/// Lightweight startup timing. Emits elapsed-millisecond checkpoints to the existing log so a slow
/// launch can be attributed to a specific phase without attaching a debugger or changing runtime code.
/// </summary>
internal static class StartupStopwatch
{
    // Starts on first use, which is the first call in App.OnStartup.
    private static readonly Stopwatch Watch = Stopwatch.StartNew();

    public static void Mark(string label) =>
        AppLog.Info($"Startup +{Watch.Elapsed.TotalMilliseconds:F0}ms: {label}");

    public static Scope Start(string label)
    {
        Mark($"begin {label}");
        return new Scope(label);
    }

    internal readonly struct Scope
    {
        private readonly Stopwatch _stopwatch;
        private readonly string _label;

        internal Scope(string label)
        {
            _label = label;
            _stopwatch = Stopwatch.StartNew();
        }

        public void Finish(string? message = null) =>
            AppLog.Info($"Startup phase '{_label}' {message}: {_stopwatch.Elapsed.TotalMilliseconds:F0}ms");
    }
}
