using System.Buffers.Binary;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace OpenTaskbar.Services;

/// <summary>Routes desktop file shortcuts to the active dock in the same Windows session.</summary>
internal static class FjordFilesActivation
{
    private const int MaximumPathBytes = 128 * 1024;
    private static readonly string PipeName = GetPipeName();

    internal static void OpenExplorerFallback(string? path)
    {
        if (GetExplorerFallbackLocation(path) is not { } location) return;

        // Launch Explorer explicitly so a user-defined folder verb cannot route the request back to Fjord.
        // Explorer treats unquoted commas as separators and a trailing backslash before a closing quote specially.
        var argument = location == FjordFilesWindow.ComputerLocation || location is [_, ':', '\\']
            ? location
            : $"\"{location.TrimEnd('\\')}\"";
        ShellShortcuts.OpenSystemTool($"explorer.exe {argument}");
    }

    internal static string? GetExplorerFallbackLocation(string? path)
    {
        var input = path?.Trim().Trim('"');
        if (string.IsNullOrWhiteSpace(input)) return null;
        if (FjordFilesWindow.IsComputerLocation(input)) return FjordFilesWindow.ComputerLocation;

        try
        {
            var expanded = Path.IsPathFullyQualified(input) ? input : Environment.ExpandEnvironmentVariables(input);
            var folder = Path.GetFullPath(expanded, Environment.GetFolderPath(Environment.SpecialFolder.UserProfile));
            return !folder.Contains('"') && Directory.Exists(folder) ? folder : null;
        }
        catch (Exception error) when (error is ArgumentException or IOException or NotSupportedException)
        {
            return null;
        }
    }

    internal static bool TrySend(string? path)
    {
        var bytes = Encoding.UTF8.GetBytes(path ?? string.Empty);
        if (bytes.Length > MaximumPathBytes) return false;

        try
        {
            using var pipe = new NamedPipeClientStream(".", PipeName, PipeDirection.Out, PipeOptions.CurrentUserOnly);
            pipe.Connect(300);
            if (GetNamedPipeServerProcessId(pipe.SafePipeHandle, out var serverProcessId))
                AllowSetForegroundWindow(serverProcessId);
            Span<byte> header = stackalloc byte[sizeof(int)];
            BinaryPrimitives.WriteInt32LittleEndian(header, bytes.Length);
            pipe.Write(header);
            pipe.Write(bytes);
            return true;
        }
        catch (Exception error) when (error is IOException or TimeoutException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    internal static async Task Listen(Action<string?> open, CancellationToken cancellationToken)
    {
        try
        {
            using var pipe = new NamedPipeServerStream(PipeName, PipeDirection.In, 1, PipeTransmissionMode.Byte,
                PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
            while (!cancellationToken.IsCancellationRequested)
            {
                await pipe.WaitForConnectionAsync(cancellationToken).ConfigureAwait(false);
                using var request = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
                request.CancelAfter(TimeSpan.FromSeconds(2));
                try
                {
                    var header = new byte[sizeof(int)];
                    await pipe.ReadExactlyAsync(header, request.Token).ConfigureAwait(false);
                    var length = BinaryPrimitives.ReadInt32LittleEndian(header);
                    if (length is < 0 or > MaximumPathBytes) continue;
                    var bytes = new byte[length];
                    await pipe.ReadExactlyAsync(bytes, request.Token).ConfigureAwait(false);
                    open(length == 0 ? null : Encoding.UTF8.GetString(bytes));
                }
                catch (IOException) { }
                catch (OperationCanceledException) when (request.IsCancellationRequested) { }
                finally { pipe.Disconnect(); }
            }
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            AppLog.Warning($"Fjord Files activation could not listen: {error.Message}");
        }
    }

    private static string GetPipeName()
    {
        using var process = Process.GetCurrentProcess();
        return $"OpenTaskbar.FjordFiles.{process.SessionId}";
    }

    [DllImport("kernel32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetNamedPipeServerProcessId(SafePipeHandle pipe, out uint serverProcessId);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool AllowSetForegroundWindow(uint processId);
}
