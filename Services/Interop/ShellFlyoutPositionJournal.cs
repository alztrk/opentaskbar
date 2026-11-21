using System.Globalization;
using System.IO;

namespace OpenTaskbar.Services;

/// <summary>A flyout handle, the process that owned it and the position Windows gave it before the dock moved it.</summary>
public sealed record FlyoutPosition(nint Handle, uint ProcessId, int Left, int Top);

/// <summary>
/// Persists the saved Start/Search/Control Center positions next to the taskbar journal. A normal exit restores
/// them in-process and deletes the file; a hard kill leaves it behind so the native watchdog can finish the job.
/// </summary>
internal static class ShellFlyoutPositionJournal
{
    private const int MaxEntries = 16;
    private static readonly string JournalDirectory = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenTaskbar");
    internal static string JournalFilePath => System.IO.Path.Combine(JournalDirectory, "flyout-positions.txt");

    internal static void Write(IEnumerable<FlyoutPosition> positions, string? path = null)
    {
        path ??= JournalFilePath;
        try
        {
            Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
            var lines = positions.Take(MaxEntries).Select(position => string.Create(CultureInfo.InvariantCulture,
                $"{(nuint)position.Handle} {position.ProcessId} {position.Left} {position.Top}"));
            var temporary = path + ".tmp";
            File.WriteAllLines(temporary, lines);
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
                var fields = line.Split(' ', StringSplitOptions.RemoveEmptyEntries);
                if (fields.Length != 4
                    || !ulong.TryParse(fields[0], NumberStyles.None, CultureInfo.InvariantCulture, out var handle)
                    || !uint.TryParse(fields[1], NumberStyles.None, CultureInfo.InvariantCulture, out var processId)
                    || !int.TryParse(fields[2], NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out var left)
                    || !int.TryParse(fields[3], NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out var top))
                {
                    continue;
                }
                if (handle != 0) positions.Add(new FlyoutPosition((nint)(nuint)handle, processId, left, top));
            }
            return positions;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or System.Security.SecurityException)
        {
            AppLog.Warning($"Flyout position journal unreadable: {ex.Message}");
            return [];
        }
    }

    internal static void Delete(string? path = null)
    {
        path ??= JournalFilePath;
        try
        {
            File.Delete(path);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or System.Security.SecurityException)
        {
            AppLog.Warning($"Flyout position journal could not be deleted: {ex.Message}");
        }
    }
}
