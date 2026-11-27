using System.ComponentModel;
using System.IO;
using System.Runtime.CompilerServices;
using System.Windows.Media.Imaging;
using System.Windows.Threading;

namespace OpenTaskbar.Models;

/// <summary>A folder shown on the dock as a stack; flags new files that arrived since it was last opened.</summary>
public sealed class DockStack : INotifyPropertyChanged, IDisposable
{
    private readonly FileSystemWatcher? _watcher;
    private bool _hasNewItems;

    public DockStack(string path, Dispatcher dispatcher)
    {
        Path = path;
        Name = System.IO.Path.GetFileName(path.TrimEnd('\\', '/')) is { Length: > 0 } name ? name : path;
        try
        {
            _watcher = new FileSystemWatcher(path)
            {
                NotifyFilter = NotifyFilters.FileName | NotifyFilters.DirectoryName,
                IncludeSubdirectories = false,
                EnableRaisingEvents = true
            };
            _watcher.Created += (_, args) =>
            {
                // Browsers create temporary download files first; ignore those until they are renamed.
                if (_disposed || IsTemporary(args.Name)) return;
                dispatcher.BeginInvoke(() =>
                {
                    if (_disposed) return;
                    HasNewItems = true;
                    ContentsChanged?.Invoke();
                });
            };
            _watcher.Renamed += (_, args) =>
            {
                if (!_disposed && !IsTemporary(args.Name))
                {
                    dispatcher.BeginInvoke(() =>
                    {
                        if (_disposed) return;
                        HasNewItems = true;
                        ContentsChanged?.Invoke();
                    });
                }
            };
            _watcher.Deleted += (_, _) =>
            {
                if (_disposed) return;
                dispatcher.BeginInvoke(() =>
                {
                    if (_disposed) return;
                    ContentsChanged?.Invoke();
                });
            };
        }
        catch (Exception ex) when (ex is ArgumentException or IOException or UnauthorizedAccessException)
        {
            _watcher = null;
        }
    }

    public string Path { get; }
    public string Name { get; }
    public event Action? ContentsChanged;
    public event PropertyChangedEventHandler? PropertyChanged;

    private BitmapSource? _icon;
    public BitmapSource? Icon
    {
        get => _icon;
        set
        {
            if (ReferenceEquals(_icon, value)) return;
            _icon = value;
            OnPropertyChanged();
        }
    }

    public bool HasNewItems
    {
        get => _hasNewItems;
        set
        {
            if (_hasNewItems == value) return;
            _hasNewItems = value;
            OnPropertyChanged();
        }
    }

    private static bool IsTemporary(string? name) => name is null
        || name.EndsWith(".crdownload", StringComparison.OrdinalIgnoreCase)
        || name.EndsWith(".part", StringComparison.OrdinalIgnoreCase)
        || name.EndsWith(".tmp", StringComparison.OrdinalIgnoreCase)
        || name.StartsWith("~$", StringComparison.Ordinal);

    /// <summary>Most recently changed entries first, folders included.</summary>
    public IReadOnlyList<FileSystemInfo> RecentEntries(int count)
    {
        try
        {
            return new DirectoryInfo(Path).EnumerateFileSystemInfos()
                .Where(entry => (entry.Attributes & (FileAttributes.Hidden | FileAttributes.System)) == 0 && !IsTemporary(entry.Name))
                .OrderByDescending(entry => entry.LastWriteTimeUtc > entry.CreationTimeUtc ? entry.LastWriteTimeUtc : entry.CreationTimeUtc)
                .Take(count)
                .ToList();
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or System.Security.SecurityException)
        {
            return [];
        }
    }

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));

    private volatile bool _disposed;

    public void Dispose()
    {
        _disposed = true;
        _watcher?.Dispose();
    }
}
