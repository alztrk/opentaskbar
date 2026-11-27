using System.Diagnostics;
using System.IO;
using System.Text;

namespace OpenTaskbar.Services;

/// <summary>
/// Lightweight, thread-safe file logger for %LOCALAPPDATA%\OpenTaskbar\log.txt.
/// Writes level-tagged, timestamped lines with the managed thread id, rotates to log.old.txt
/// at 1 MB (including at runtime, not only on startup), and keeps failures as structured
/// multi-line records so the UI never has to display them.
/// Set the OT_VERBOSE_LOG environment variable to 1 to include Debug/Trace output.
/// </summary>
public static class AppLog
{
    private const long MaxBytes = 1024 * 1024;
    private static readonly object Gate = new();
    private static StreamWriter? _writer;
    private static string? _logPath;
    private static bool _verbose;

    public static string LogPath => _logPath ??= Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenTaskbar", "log.txt");

    public static void Initialize()
    {
        lock (Gate)
        {
            if (_writer is not null) return;
            _verbose = string.Equals(Environment.GetEnvironmentVariable("OT_VERBOSE_LOG"), "1", StringComparison.Ordinal);
            try
            {
                Directory.CreateDirectory(Path.GetDirectoryName(LogPath)!);
                // Rotation is handled on the first Write below (the stream must be open first).
                _writer = OpenWriter();
                Info($"OpenTaskbar {typeof(AppLog).Assembly.GetName().Version} started.");
            }
            catch (IOException) { _writer = null; }
            catch (UnauthorizedAccessException) { _writer = null; }
        }
    }

    private static StreamWriter OpenWriter()
    {
        var stream = new FileStream(LogPath, FileMode.Append, FileAccess.Write, FileShare.ReadWrite);
        // BOM on new files lets Notepad and Windows PowerShell read Turkish text correctly.
        var encoding = new UTF8Encoding(encoderShouldEmitUTF8Identifier: stream.Length == 0);
        return new StreamWriter(stream, encoding) { AutoFlush = true };
    }

    private static void RotateIfNeeded()
    {
        try
        {
            if (_writer is null || _writer.BaseStream.Length <= MaxBytes) return;
            // The current FileStream keeps the file locked; close it before moving, then reopen.
            _writer.Flush();
            _writer.Dispose();
            _writer = null;
            File.Move(LogPath, Path.ChangeExtension(LogPath, ".old.txt"), overwrite: true);
            _writer = OpenWriter();
        }
        catch (IOException) { }
        catch (UnauthorizedAccessException) { }
        finally
        {
            if (_writer is null)
            {
                try { _writer = OpenWriter(); }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
            }
        }
    }

    public static void Info(string message) => Write("INFO", message);
    public static void Warning(string message) => Write("WARN", message);
    public static void Error(string message) => Write("ERROR", message);
    public static void Debug(string message)
    {
        if (_verbose) Write("DEBUG", message);
    }

    private static void Write(string level, string message)
    {
        lock (Gate)
        {
            if (_writer is null) return;
            try
            {
                RotateIfNeeded(); // runtime rotation, not just on startup
                if (_writer is null) return;
                var line = $"{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff} [{level,-5}][T{Environment.CurrentManagedThreadId,2}] {message}";
                _writer.WriteLine(line);
            }
            catch (IOException)
            {
                // Logging must never take down the app.
            }
            catch (ObjectDisposedException) { }
        }
    }

    /// <summary>
    /// Resolves a localization key the same way the log records it. Already-resolved text and raw exception
    /// messages are returned unchanged instead of throwing a missing-key exception.
    /// </summary>
    public static string ResolveMessage(string messageKey, params object[] arguments)
    {
        if (string.IsNullOrWhiteSpace(messageKey)) return string.Empty;
        try
        {
            return arguments.Length == 0 ? UiText.Get(messageKey) : UiText.Format(messageKey, arguments);
        }
        catch
        {
            return messageKey;
        }
    }

    /// <summary>Writes a failure as one structured record: localization key, message and exception details.</summary>
    public static void ReportError(string messageKey) => WriteError(messageKey, [], null);

    public static void ReportError(string messageKey, params object[] arguments) => WriteError(messageKey, arguments, null);

    public static void ReportError(string messageKey, Exception exception) => WriteError(messageKey, [], exception);

    public static void ReportError(string messageKey, Exception exception, params object[] arguments) =>
        WriteError(messageKey, arguments, exception);

    private static void WriteError(string messageKey, object[] arguments, Exception? exception)
    {
        var sb = new StringBuilder();
        sb.Append("Failure key=").Append(messageKey)
          .Append(" | message=").Append(ResolveMessage(messageKey, arguments));
        if (exception is not null)
        {
            sb.AppendLine();
            sb.Append("  exception=").Append(exception.GetType().Name).Append(": ").Append(exception.Message);
            if (exception.InnerException is not null)
            {
                sb.AppendLine();
                sb.Append("  inner=").Append(exception.InnerException.GetType().Name).Append(": ").Append(exception.InnerException.Message);
            }
            if (!string.IsNullOrWhiteSpace(exception.StackTrace))
            {
                sb.AppendLine();
                sb.Append("  stack=").Append(exception.StackTrace);
            }
        }
        Error(sb.ToString());
    }

    /// <summary>Opens log.txt in the default text editor for users who need to inspect recorded failures.</summary>
    public static bool TryOpenLogFile()
    {
        try
        {
            if (!File.Exists(LogPath))
            {
                Warning($"Log file not found: {LogPath}");
                return false;
            }

            using var process = Process.Start(new ProcessStartInfo(LogPath) { UseShellExecute = true });
            return process is not null;
        }
        catch (Exception ex)
        {
            Warning($"Cannot open log file: {ex.Message}");
            return false;
        }
    }

    public static void Shutdown()
    {
        lock (Gate)
        {
            _writer?.Flush();
            _writer?.Dispose();
            _writer = null;
        }
    }
}
