using System.IO;
using System.Text;
using System.Text.Json.Serialization;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Windows.Media.Imaging;

namespace OpenTaskbar.Models;

public sealed class DockApp(string name, string executablePath) : INotifyPropertyChanged
{
    public string Name { get; } = name;
    public string ExecutablePath { get; } = executablePath;
    public event PropertyChangedEventHandler? PropertyChanged;
    private bool _hasNotification;
    private bool _isForeground;
    private int _windowCount;
    private BitmapSource? _icon;
    public string? LaunchArguments { get; init; }

    [JsonIgnore]
    public string? ShortcutPath { get; init; }

    [JsonIgnore]
    public string? IconLocation { get; init; }

    [JsonIgnore]
    public string? AppUserModelId { get; init; }

    /// <summary>
    /// Stable unique identity for dock ordering, pin order, drag-and-drop, groups, and profiles.
    /// Plain desktop apps keep <see cref="ExecutablePath"/> for full compatibility with existing saved settings,
    /// while multi-argument shortcuts (such as Edge/Chrome PWAs sharing msedge_proxy.exe) and shared UWP host
    /// processes include their normalized arguments or AUMID so distinct pins never collide.
    /// </summary>
    [JsonIgnore]
    public string DockKey => BuildDockKey(ExecutablePath, LaunchArguments, AppUserModelId);

    internal static string BuildDockKey(string executablePath, string? launchArguments, string? appUserModelId = null)
    {
        var normalizedArgs = NormalizeArguments(launchArguments);
        if (normalizedArgs.Length > 0)
        {
            return $"{executablePath}|{normalizedArgs}";
        }

        if (!string.IsNullOrWhiteSpace(appUserModelId) && IsSharedHostExecutable(executablePath))
        {
            return $"{executablePath}|{appUserModelId.Trim()}";
        }

        return executablePath;
    }

    internal static string NormalizeArguments(string? arguments)
    {
        if (string.IsNullOrWhiteSpace(arguments)) return string.Empty;
        var span = arguments.AsSpan().Trim();
        var sb = new StringBuilder(span.Length);
        var inQuotes = false;
        var lastWasSpace = false;
        foreach (var ch in span)
        {
            if (ch == '"')
            {
                inQuotes = !inQuotes;
                sb.Append(ch);
                lastWasSpace = false;
            }
            else if (!inQuotes && char.IsWhiteSpace(ch))
            {
                if (!lastWasSpace)
                {
                    sb.Append(' ');
                    lastWasSpace = true;
                }
            }
            else
            {
                sb.Append(ch);
                lastWasSpace = false;
            }
        }
        return sb.ToString();
    }

    internal static bool IsSharedHostExecutable(string? executablePath)
    {
        if (string.IsNullOrWhiteSpace(executablePath)) return false;
        var fileName = Path.GetFileName(executablePath);
        return string.Equals(fileName, "ApplicationFrameHost.exe", StringComparison.OrdinalIgnoreCase)
            || string.Equals(fileName, "WWAHost.exe", StringComparison.OrdinalIgnoreCase);
    }

    internal static bool IsSystemPackagedInstallPath(string? executablePath)
    {
        if (string.IsNullOrWhiteSpace(executablePath)) return false;
        return ContainsPathSegment(executablePath, "SystemApps")
            || ContainsPathSegment(executablePath, "ImmersiveControlPanel");
    }

    internal static bool IsPackagedInstallPath(string? executablePath)
    {
        if (string.IsNullOrWhiteSpace(executablePath)) return false;
        return ContainsPathSegment(executablePath, "WindowsApps")
            || IsSystemPackagedInstallPath(executablePath);
    }

    internal static bool IsSelfExecutable(string? executablePath, string? currentProcessPath = null)
    {
        if (string.IsNullOrWhiteSpace(executablePath)) return false;
        var trimmed = executablePath.Trim();
        var processPath = !string.IsNullOrWhiteSpace(currentProcessPath)
            ? currentProcessPath.Trim()
            : Environment.ProcessPath;
        if (!string.IsNullOrWhiteSpace(processPath))
        {
            try
            {
                if (string.Equals(Path.GetFullPath(trimmed), Path.GetFullPath(processPath), StringComparison.OrdinalIgnoreCase))
                {
                    return true;
                }
            }
            catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
            {
                // Fall through to filename comparison.
            }
        }

        return string.Equals(Path.GetFileName(trimmed), "OpenTaskbar.exe", StringComparison.OrdinalIgnoreCase);
    }

    private static bool ContainsPathSegment(string path, string segment) =>
        path.Contains($"\\{segment}\\", StringComparison.OrdinalIgnoreCase)
        || path.Contains($"/{segment}/", StringComparison.OrdinalIgnoreCase);

    // Notifies because packaged apps can receive their icon after the dock entry is created.
    [JsonIgnore]
    public BitmapSource? Icon { get => _icon; set => SetField(ref _icon, value); }

    [JsonIgnore]
    public bool HasNotification { get => _hasNotification; set => SetField(ref _hasNotification, value); }

    [JsonIgnore]
    public bool IsRunning => WindowCount > 0;

    [JsonIgnore]
    public bool IsForeground { get => _isForeground; set => SetField(ref _isForeground, value); }

    [JsonIgnore]
    public int WindowCount
    {
        get => _windowCount;
        set
        {
            if (SetField(ref _windowCount, value))
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsRunning)));
        }
    }

    private bool SetField<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return false;
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
        return true;
    }
}
