using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;

using OpenTaskbar.Services;

namespace OpenTaskbar;

public partial class FjordFilesWindow : Window
{
    internal const string ComputerLocation = "shell:MyComputerFolder";
    private const string ComputerClassId = "{20d04fe0-3aea-1069-a2d8-08002b30309d}";
    private readonly List<string> _history = [];
    private readonly string _initialPath;
    private int _historyIndex = -1;
    private CancellationTokenSource? _navigation;
    private CancellationTokenSource? _preview;
    private Window? _quickLook;
    private FileEntry[] _items = [];
    private string? _folder;
    private bool _loading;

    internal FjordFilesWindow(string? initialPath, DisplayInfo? display = null)
    {
        _initialPath = initialPath ?? ComputerLocation;
        InitializeComponent();
        if (display is not null) CenterOnDisplay(display);
        foreach (var (label, glyph, path) in GetFavorites())
            if (!string.IsNullOrEmpty(path)) AddPlace(Favorites, label, glyph, path);
        foreach (var drive in DriveInfo.GetDrives())
            AddPlace(Volumes, drive.Name.TrimEnd('\\'), "\uEDA2", drive.RootDirectory.FullName);
        Loaded += async (_, _) => await Navigate(_initialPath);
        Closed += (_, _) =>
        {
            _navigation?.Cancel();
            _preview?.Cancel();
        };
        UpdateButtons();
    }

    internal static void Open(string? path, DisplayInfo? display = null)
    {
        var window = new FjordFilesWindow(path, display);
        window.Show();
        NativeWindowTracker.Activate(new WindowInteropHelper(window).Handle);
    }

    internal static void CloseAll()
    {
        foreach (var window in Application.Current.Windows.OfType<FjordFilesWindow>().ToArray())
            window.Close();
    }

    private void CenterOnDisplay(DisplayInfo display)
    {
        WindowStartupLocation = WindowStartupLocation.Manual;
        var area = display.WorkArea;
        Width = Math.Clamp(Width, MinWidth, Math.Max(MinWidth, area.Width));
        Height = Math.Clamp(Height, MinHeight, Math.Max(MinHeight, area.Height));
        Left = area.Left + Math.Max(0, (area.Width - Width) / 2);
        Top = area.Top + Math.Max(0, (area.Height - Height) / 2);
        SourceInitialized += (_, _) =>
        {
            var scale = display.Dpi / 96d;
            SetWindowPos(new WindowInteropHelper(this).Handle, 0,
                (int)Math.Round(Left * scale), (int)Math.Round(Top * scale),
                (int)Math.Round(Width * scale), (int)Math.Round(Height * scale), 0x0014);
        };
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint window, nint after, int x, int y, int width, int height, uint flags);

    private static IEnumerable<(string, string, string)> GetFavorites()
    {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        yield return (FjordFilesText.Get("Computer"), "\uE770", ComputerLocation);
        yield return (FjordFilesText.Get("Home"), "\uE80F", home);
        yield return (FjordFilesText.Get("Desktop"), "\uE7F4", Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory));
        yield return (FjordFilesText.Get("Downloads"), "\uE896", DownloadsPath(home));
        yield return (FjordFilesText.Get("Documents"), "\uE8A5", Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments));
        yield return (FjordFilesText.Get("Pictures"), "\uEB9F", Environment.GetFolderPath(Environment.SpecialFolder.MyPictures));
        yield return (FjordFilesText.Get("Music"), "\uE8D6", Environment.GetFolderPath(Environment.SpecialFolder.MyMusic));
        yield return (FjordFilesText.Get("Videos"), "\uE8B2", Environment.GetFolderPath(Environment.SpecialFolder.MyVideos));
    }

    private void AddPlace(Panel section, string name, string glyph, string path)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal };
        row.Children.Add(new TextBlock { Text = glyph, FontFamily = new FontFamily("Segoe MDL2 Assets"),
            Foreground = Brushes.LightSkyBlue, Width = 24, VerticalAlignment = VerticalAlignment.Center });
        row.Children.Add(new TextBlock { Text = name, VerticalAlignment = VerticalAlignment.Center });
        // A Grid stretches across the button's content area so names share a left edge.
        var content = new Grid { Width = 136 };
        content.Children.Add(row);
        var button = new Button { Content = content, ToolTip = path, Margin = new Thickness(0, 1, 0, 1) };
        AutomationProperties.SetName(button, name);
        button.Click += async (_, _) => await Navigate(path);
        section.Children.Add(button);
    }

    private async Task Navigate(string target, int? historyIndex = null)
    {
        _navigation?.Cancel();
        using var request = new CancellationTokenSource();
        _navigation = request;
        _loading = true;
        Empty.Text = "";
        Status.Text = FjordFilesText.Get("Loading");
        UpdateButtons();
        try
        {
            var input = target.Trim().Trim('"');
            var expanded = Path.IsPathFullyQualified(input) ? input : Environment.ExpandEnvironmentVariables(input);
            var path = IsComputerLocation(expanded) ? ComputerLocation
                : Path.GetFullPath(expanded, _folder is not null && !IsComputerLocation(_folder)
                    ? _folder : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile));
            var items = await Task.Run(() => path == ComputerLocation ? ReadDrives() : ReadEntries(path, request.Token), request.Token);
            if (request.IsCancellationRequested) return;
            _folder = path;
            _items = items;
            if (historyIndex.HasValue)
                _historyIndex = historyIndex.Value;
            else if (_historyIndex < 0 || !StringComparer.OrdinalIgnoreCase.Equals(_history[_historyIndex], path))
            {
                _history.RemoveRange(_historyIndex + 1, _history.Count - _historyIndex - 1);
                _history.Add(path);
                _historyIndex++;
            }
            Location.Text = path == ComputerLocation ? FjordFilesText.Get("Computer") : path;
            Breadcrumb.Text = path == ComputerLocation ? FjordFilesText.Get("Computer") : FolderTrail(path);
            if (Search.Text.Length > 0) Search.Clear();
            else ApplyFilter();
        }
        catch (OperationCanceledException) when (request.IsCancellationRequested) { }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException or NotSupportedException)
        {
            var message = error switch
            {
                UnauthorizedAccessException => FjordFilesText.Get("AccessDenied"),
                DirectoryNotFoundException or ArgumentException => FjordFilesText.Get("Missing"),
                _ => FjordFilesText.Get("OpenFolderFailed")
            };
            Status.Text = message;
            if (_folder is null) Empty.Text = message;
        }
        finally
        {
            if (ReferenceEquals(_navigation, request))
            {
                _navigation = null;
                _loading = false;
                UpdateButtons();
            }
        }
    }

    internal static bool IsComputerLocation(string path) =>
        path.Equals(ComputerLocation, StringComparison.OrdinalIgnoreCase)
        || path.Equals("Bu Bilgisayar", StringComparison.OrdinalIgnoreCase)
        || path.Equals("This PC", StringComparison.OrdinalIgnoreCase)
        || path.Equals("::" + ComputerClassId, StringComparison.OrdinalIgnoreCase)
        || path.Equals("shell:::" + ComputerClassId, StringComparison.OrdinalIgnoreCase);

    private static FileEntry[] ReadDrives() => DriveInfo.GetDrives()
        .Select(drive => new FileEntry(drive.Name, drive.Name.TrimEnd('\\'), true, null, null))
        .ToArray();

    private static string DownloadsPath(string home)
    {
        if (SHGetKnownFolderPath(new Guid("374DE290-123F-4565-9164-39C4925E467B"), 0, 0, out var path) != 0)
            return Path.Combine(home, "Downloads");
        try { return Marshal.PtrToStringUni(path) ?? Path.Combine(home, "Downloads"); }
        finally { Marshal.FreeCoTaskMem(path); }
    }

    [DllImport("shell32.dll")]
    private static extern int SHGetKnownFolderPath(in Guid id, uint flags, nint token, out nint path);

    private void OpenFile(string path, string? argument = null)
    {
        try
        {
            var launch = new ProcessStartInfo(path) { UseShellExecute = true };
            if (argument is not null) launch.ArgumentList.Add(argument);
            using var process = Process.Start(launch);
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException)
        {
            Status.Text = FjordFilesText.Get("OpenItemFailed");
        }
    }

    private static FileEntry[] ReadEntries(string path, CancellationToken token)
    {
        var entries = new List<FileEntry>();
        foreach (var item in new DirectoryInfo(path).EnumerateFileSystemInfos())
        {
            token.ThrowIfCancellationRequested();
            try { entries.Add(FileEntry.Read(item)); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }
        return entries.OrderByDescending(item => item.IsFolder).ThenBy(item => item.Name, StringComparer.Create(FjordFilesText.Culture, true)).ToArray();
    }

    private static string FolderTrail(string path)
    {
        var folder = new DirectoryInfo(path);
        var parts = new List<string>();
        while (parts.Count < 3)
        {
            parts.Add(folder.Name);
            if (folder.Parent is not { } parent) break;
            folder = parent;
            if (parts.Count == 3) parts.Add("…");
        }
        parts.Reverse();
        return string.Join("   ›   ", parts);
    }

    private void ApplyFilter()
    {
        var term = Search.Text.Trim();
        var visible = term.Length == 0 ? _items : _items.Where(item =>
            FjordFilesText.Culture.CompareInfo.IndexOf(item.Name, term, CompareOptions.IgnoreCase) >= 0).ToArray();
        Entries.ItemsSource = visible;
        Empty.Text = visible.Length > 0 ? "" : FjordFilesText.Get(term.Length == 0 ? "Empty" : "NoMatches");
        Status.Text = term.Length == 0 ? FjordFilesText.Format("Count", visible.Length)
            : FjordFilesText.Format("FilteredCount", visible.Length, _items.Length);
    }

    private void UpdateButtons()
    {
        BackButton.IsEnabled = !_loading && _historyIndex > 0;
        ForwardButton.IsEnabled = !_loading && _historyIndex + 1 < _history.Count;
        UpButton.IsEnabled = !_loading && _folder is not null && _folder != ComputerLocation;
    }

    private Task StepHistory(int step)
    {
        var next = _historyIndex + step;
        return next >= 0 && next < _history.Count ? Navigate(_history[next], next) : Task.CompletedTask;
    }

    private Task Up() => _folder is not null && _folder != ComputerLocation
        ? Navigate(Directory.GetParent(_folder)?.FullName ?? ComputerLocation) : Task.CompletedTask;

    private async Task OpenSelection()
    {
        if (Entries.SelectedItem is not FileEntry item) return;
        if (item.IsFolder) await Navigate(item.Path);
        else OpenFile(item.Path);
    }

    private void TogglePreview()
    {
        PreviewPane.Visibility = PreviewPane.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;
        _ = RefreshPreview();
    }

    private async Task RefreshPreview()
    {
        _preview?.Cancel();
        if (PreviewPane.Visibility != Visibility.Visible) return;
        using var request = new CancellationTokenSource();
        _preview = request;
        try
        {
            var control = await FjordFilePreview.Build(Entries.SelectedItem as FileEntry, request.Token);
            if (!request.IsCancellationRequested) PreviewPane.Child = control;
        }
        catch (OperationCanceledException) when (request.IsCancellationRequested) { }
        finally
        {
            if (ReferenceEquals(_preview, request)) _preview = null;
        }
    }

    private async Task QuickLook()
    {
        if (Entries.SelectedItem is not FileEntry item) return;
        if (_quickLook is { } existing) { existing.Activate(); return; }
        using var request = new CancellationTokenSource();
        var frame = new Border { Padding = new Thickness(20), Child = new TextBlock { Text = FjordFilesText.Get("PreparingPreview") } };
        var window = new Window
        {
            Title = FjordFilesText.Get("QuickLook"), Owner = this, Width = 710, Height = 510, MinWidth = 360, MinHeight = 260,
            WindowStartupLocation = WindowStartupLocation.CenterOwner, Content = frame, ShowInTaskbar = false,
            Background = Background, Foreground = Foreground, FontFamily = FontFamily, FontSize = 14
        };
        window.PreviewKeyDown += (_, key) =>
        {
            if (key.Key is Key.Space or Key.Escape) { key.Handled = true; window.Close(); }
        };
        var closed = new TaskCompletionSource();
        window.Closed += (_, _) =>
        {
            _quickLook = null;
            request.Cancel();
            closed.TrySetResult();
        };
        _quickLook = window;
        window.Show();
        try
        {
            var preview = await FjordFilePreview.Build(item, request.Token);
            if (!request.IsCancellationRequested) frame.Child = preview;
        }
        catch (OperationCanceledException) when (request.IsCancellationRequested) { }
        await closed.Task;
    }

    private async void WindowKeyDown(object sender, KeyEventArgs e)
    {
        var shortcut = (Keyboard.Modifiers, e.SystemKey == Key.None ? e.Key : e.SystemKey);
        if (shortcut is (ModifierKeys.Control, Key.L or Key.F)
            or (ModifierKeys.Alt, Key.Left or Key.Right or Key.Up)
            or (ModifierKeys.None, Key.F4 or Key.F5)
            || Entries.IsKeyboardFocusWithin && shortcut is (ModifierKeys.None, Key.Enter or Key.Space))
            e.Handled = true;
        switch (shortcut)
        {
            case (ModifierKeys.Control, Key.L): Location.Focus(); Location.SelectAll(); break;
            case (ModifierKeys.Control, Key.F): Search.Focus(); Search.SelectAll(); break;
            case (ModifierKeys.Alt, Key.Left): await StepHistory(-1); break;
            case (ModifierKeys.Alt, Key.Right): await StepHistory(1); break;
            case (ModifierKeys.Alt, Key.Up): await Up(); break;
            case (ModifierKeys.None, Key.F4): TogglePreview(); break;
            case (ModifierKeys.None, Key.F5) when _folder is not null: await Navigate(_folder); break;
            case (ModifierKeys.None, Key.Enter) when Entries.IsKeyboardFocusWithin: await OpenSelection(); break;
            case (ModifierKeys.None, Key.Space) when Entries.IsKeyboardFocusWithin: _ = QuickLook(); break;
        }
    }

    private async void BackClick(object sender, RoutedEventArgs e) => await StepHistory(-1);
    private async void ForwardClick(object sender, RoutedEventArgs e) => await StepHistory(1);
    private async void UpClick(object sender, RoutedEventArgs e) => await Up();
    private void PreviewClick(object sender, RoutedEventArgs e) => TogglePreview();
    private void MinimizeClick(object sender, RoutedEventArgs e) => WindowState = WindowState.Minimized;
    private void MaximizeClick(object sender, RoutedEventArgs e) => WindowState = WindowState == WindowState.Maximized ? WindowState.Normal : WindowState.Maximized;
    private void CloseClick(object sender, RoutedEventArgs e) => Close();
    private void SearchChanged(object sender, TextChangedEventArgs e) { if (_folder is not null) ApplyFilter(); }
    private void EntrySelectionChanged(object sender, SelectionChangedEventArgs e) { _ = RefreshPreview(); }
    private async void EntryDoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is DependencyObject source
            && ItemsControl.ContainerFromElement(Entries, source) is ListBoxItem && e.ChangedButton == MouseButton.Left)
            await OpenSelection();
    }
    private async void OpenItemClick(object sender, RoutedEventArgs e) => await OpenSelection();
    private async void QuickLookClick(object sender, RoutedEventArgs e) => await QuickLook();
    private void CopyPathClick(object sender, RoutedEventArgs e)
    {
        if (Entries.SelectedItem is not FileEntry item) return;
        try { Clipboard.SetText(item.Path); }
        catch (System.Runtime.InteropServices.ExternalException) { Status.Text = FjordFilesText.Get("CopyFailed"); return; }
        Status.Text = FjordFilesText.Get("Copied");
    }
    private void RevealClick(object sender, RoutedEventArgs e)
    {
        if (Entries.SelectedItem is FileEntry item) OpenFile("explorer.exe", "/select," + item.Path);
    }
    private async void LocationKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Enter) { e.Handled = true; await Navigate(Location.Text); }
        else if (e.Key == Key.Escape) { Location.Text = _folder == ComputerLocation ? FjordFilesText.Get("Computer") : _folder ?? ""; e.Handled = true; }
    }
}

internal sealed record FileEntry(string Path, string Name, bool IsFolder, long? Size, DateTime? Changed)
{
    public string DateLabel => Changed?.ToString("dd MMM yyyy", FjordFilesText.Culture) ?? "";
    public string SizeLabel => Size switch
    {
        null => "",
        < 1024 => $"{Size:N0} B",
        < 1048576 => $"{Size / 1024d:N1} KB",
        < 1073741824 => $"{Size / 1048576d:N1} MB",
        _ => $"{Size / 1073741824d:N1} GB"
    };

    internal static FileEntry Read(FileSystemInfo info) =>
        new(info.FullName, info.Name, info is DirectoryInfo, (info as FileInfo)?.Length, info.LastWriteTime);
}
