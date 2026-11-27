using System.Globalization;
using System.IO;
using System.Text;

namespace OpenTaskbar.Services;

/// <summary>A flyout handle, the process that owned it and the position Windows gave it before the dock moved it.</summary>
public sealed record FlyoutPosition(nint Handle, uint ProcessId, int Left, int Top);

/// <summary>
/// Persists the saved Start/Search/Control Center positions next to the taskbar journal. A normal exit restores
/// them in-process and deletes the file; a hard kill leaves it behind so the native watchdog can finish the job.
///
/// Wire format is shared with the Rust <c>flyout_writer</c>/<c>flyout_journal</c> and the C watchdog: at most
/// 16 lines of <c>&lt;hwnd&gt; &lt;pid&gt; &lt;left&gt; &lt;top&gt;</c>, invariant UTF-8 without BOM and LF endings.
/// An optional <c>generation=&lt;n&gt; seq=&lt;n&gt;</c> suffix written by Rust is accepted but ignored by the
/// watchdog, which only scans the first four fields with <c>fwscanf("%llu %u %d %d")</c>.
/// </summary>
internal static class ShellFlyoutPositionJournal
{
    internal const int MaxEntries = 16;
    internal const string JournalFileName = "flyout-positions.txt";
    // Must stay byte-identical to Rust `flyout_journal::tmp_path`: appending ".tmp" to the
    // whole file name yields "flyout-positions.txt.tmp" (NOT "flyout-positions.tmp").
    internal const string TemporaryExtension = ".txt.tmp";

    private static readonly string JournalDirectory = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenTaskbar");
    internal static string JournalFilePath => Path.Combine(JournalDirectory, JournalFileName);

    /// <summary>The single atomic-write target used by both C# and Rust flyout writers.</summary>
    internal static string TemporaryFilePath(string? path = null) =>
        Path.ChangeExtension(path ?? JournalFilePath, TemporaryExtension);

    /// <summary>
    /// A journal line the C watchdog can parse with <c>%llu %u %d %d</c>. Kept invariant so a Turkish locale
    /// cannot turn a negative coordinate into a localized number.
    /// </summary>
    internal static string FormatLine(FlyoutPosition position) => string.Create(CultureInfo.InvariantCulture,
        $"{(nuint)position.Handle} {position.ProcessId} {position.Left} {position.Top}");

    internal static void Write(IEnumerable<FlyoutPosition> positions, string? path = null)
    {
        path ??= JournalFilePath;
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            var lines = positions.Take(MaxEntries).Where(p => p.Handle != 0).Select(FormatLine).ToArray();
            var temporary = TemporaryFilePath(path);
            File.WriteAllText(temporary,
                lines.Length == 0 ? string.Empty : string.Join('\n', lines) + "\n",
                new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
            File.Move(temporary, path, overwrite: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or System.Security.SecurityException)
        {
            AppLog.Warning($"Flyout position journal unavailable: {ex.Message}");
        }
    }

    internal static IReadOnlyList<FlyoutPosition> Read(string? path = null)
    {
        path ??= JournalFilePath;
        try
        {
            if (!File.Exists(path)) return [];
            var positions = new List<FlyoutPosition>(MaxEntries);
            foreach (var line in File.ReadLines(path).Take(MaxEntries))
            {
                if (TryParseLine(line, out var position)) positions.Add(position);
            }
            return positions;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or System.Security.SecurityException)
        {
            AppLog.Warning($"Flyout position journal unreadable: {ex.Message}");
            return [];
        }
    }

    /// <summary>
    /// Parses both the four-field watchdog form and the optional Rust/C# metadata suffix. Metadata must be
    /// complete and numeric: a half-written pair is as unusable as a truncated line.
    /// </summary>
    internal static bool TryParseLine(string line, out FlyoutPosition position)
    {
        position = default!;
        var fields = line.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        if (fields.Length is not (4 or 6)
            || !ulong.TryParse(fields[0], NumberStyles.None, CultureInfo.InvariantCulture, out var handle)
            || !uint.TryParse(fields[1], NumberStyles.None, CultureInfo.InvariantCulture, out var processId)
            || !int.TryParse(fields[2], NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out var left)
            || !int.TryParse(fields[3], NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out var top))
        {
            return false;
        }
        if (fields.Length == 6
            && (!TryParseMeta(fields[4], "generation", out _)
                || !TryParseMeta(fields[5], "seq", out _)))
        {
            return false;
        }
        // HWND 0 is never restorable and must not move an unrelated window.
        if (handle == 0) return false;
        position = new FlyoutPosition((nint)(nuint)handle, processId, left, top);
        return true;
    }

    private static bool TryParseMeta(string field, string key, out ulong value)
    {
        value = 0;
        return field.StartsWith(key + "=", StringComparison.Ordinal)
            && ulong.TryParse(field[(key.Length + 1)..], NumberStyles.None, CultureInfo.InvariantCulture, out value);
    }

    /// <summary>
    /// Windows recycles HWNDs: a journal entry may only move a window that still belongs to the recorded PID.
    /// </summary>
    internal static bool IsCurrent(FlyoutPosition position, Func<nint, uint> processIdOf, Func<nint, bool> windowExists) =>
        position.Handle != 0 && position.ProcessId != 0
        && windowExists(position.Handle) && processIdOf(position.Handle) == position.ProcessId;

    internal static IReadOnlyList<FlyoutPosition> ReadValid(
        Func<nint, uint> processIdOf,
        Func<nint, bool> windowExists,
        string? path = null) =>
        Read(path).Where(position => IsCurrent(position, processIdOf, windowExists)).ToArray();

    internal static void Delete(string? path = null)
    {
        path ??= JournalFilePath;
        try
        {
            File.Delete(path);
            // A stale temporary file from a killed writer must never be mistaken for a newer journal.
            File.Delete(TemporaryFilePath(path));
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or System.Security.SecurityException)
        {
            AppLog.Warning($"Flyout position journal could not be deleted: {ex.Message}");
        }
    }
}
