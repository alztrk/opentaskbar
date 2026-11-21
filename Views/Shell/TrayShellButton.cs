using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Services;

namespace OpenTaskbar.Views.Shell;

/// <summary>
/// Permanent notification-area button. Stands in for the hidden tray: lists running background apps that own a
/// tray icon. The rich flyout logic used to live in <c>TrayWidget</c>; it is now hosted directly by a dock button.
/// </summary>
public sealed class TrayShellButton : Button
{
    private readonly DockFlyout _flyout = new();
    private int _requestVersion;
    private IReadOnlyList<TrayApp>? _lastApps;
    private Task<IReadOnlyList<TrayApp>>? _refresh;
    private Action<string>? _reportError;

    /// <summary>Seam for tests: enumerates the tray apps (registry snapshot plus every running process).</summary>
    internal Func<IReadOnlyList<TrayApp>> AppsReader { get; set; } = TrayIcons.RunningApps;

    public event Action<string>? CloseApplicationRequested;
    public event Action<string>? TerminateApplicationRequested;

    public TrayShellButton()
    {
        // Same Grid + TransformGroup cell as the other shell buttons so magnification and edge-origin updates
        // can drive the existing transforms instead of allocating one on first hover.
        var glyph = new ContentControl
        {
            Content = FindResource("IconChevronUp"),
            Width = (double)Application.Current.Resources["DockUtilityGlyphSize"],
            Height = (double)Application.Current.Resources["DockUtilityGlyphSize"],
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center
        };
        glyph.SetResourceReference(StyleProperty, "WidgetIcon");
        glyph.SetResourceReference(ForegroundProperty, "TextPrimary");
        var cell = new Grid
        {
            Width = (double)Application.Current.Resources["DockIconSize"],
            Height = (double)Application.Current.Resources["DockIconSize"],
            RenderTransformOrigin = new Point(0.5, 0.5)
        };
        cell.RenderTransform = new TransformGroup
        {
            Children =
            {
                new ScaleTransform(1, 1),
                new TranslateTransform()
            }
        };
        cell.Children.Add(glyph);
        Content = cell;
        Click += Button_Click;
        // Warm the list while the pointer approaches, so a click can show it at once.
        MouseEnter += (_, _) => _ = RefreshAppsQuietlyAsync();
        Unloaded += (_, _) =>
        {
            UiText.Current.PropertyChanged -= UiText_PropertyChanged;
            _flyout.Close();
        };
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        Localize();
    }

    private void UiText_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        Localize();
        if (_flyout.IsOpen && _lastApps is not null)
            _flyout.Show(this, BuildContent(_lastApps), animate: false);
    }

    /// <summary>Wires the dock's error reporter and process actions. Called once from MainWindow.</summary>
    public void Initialize(Action<string> reportError,
        Action<string> closeApplication, Action<string> terminateApplication)
    {
        _reportError = reportError;
        CloseApplicationRequested += closeApplication;
        TerminateApplicationRequested += terminateApplication;
    }

    private void Report(string? message) => _reportError?.Invoke(message ?? string.Empty);

    /// <summary>
    /// One enumeration at a time, shared by every caller. It reads the tray registry snapshot and every running
    /// process (~100 ms), so it always runs off the UI thread and its result is kept for the next click.
    /// </summary>
    private Task<IReadOnlyList<TrayApp>> RefreshAppsAsync()
    {
        if (_refresh is { IsCompleted: false } running) return running;
        var reader = AppsReader;
        return _refresh = Task.Run(reader);
    }

    private async Task RefreshAppsQuietlyAsync()
    {
        try
        {
            await RefreshAppsAsync();
        }
        catch (Exception ex) when (ex is System.IO.IOException or UnauthorizedAccessException
            or System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            AppLog.Warning($"Tray enumeration failed: {ex.Message}");
        }
    }

    internal static bool SameApps(IReadOnlyList<TrayApp> left, IReadOnlyList<TrayApp> right) =>
        left.Count == right.Count && left.Zip(right).All(pair =>
            pair.First.Name == pair.Second.Name
            && string.Equals(pair.First.ExecutablePath, pair.Second.ExecutablePath, StringComparison.OrdinalIgnoreCase));

    private void Localize()
    {
        ToolTip = UiText.Get("TrayApps");
        System.Windows.Automation.AutomationProperties.SetName(this, UiText.Get("TrayApps"));
    }

    private async void Button_Click(object sender, RoutedEventArgs e)
    {
        if (_flyout.IsOpen || _flyout.RecentlyClosed(this))
        {
            _flyout.Close();
            return;
        }

        var requestVersion = ++_requestVersion;
        if (TrayHost.Current is { IsRunning: true } host)
        {
            _flyout.Show(this, BuildLiveContent(host.AppIcons()));
            return;
        }

        // Show the last known list at once instead of waiting for a fresh enumeration; update it in place if the
        // fresh list differs. Only the very first open, with nothing cached yet, waits for the enumeration.
        var cached = _lastApps;
        if (cached is not null) _flyout.Show(this, BuildContent(cached));
        try
        {
            var apps = await RefreshAppsAsync();
            _lastApps = apps;
            if (requestVersion != _requestVersion) return;
            if (cached is null) _flyout.Show(this, BuildContent(apps));
            else if (_flyout.IsOpen && !SameApps(cached, apps)) _flyout.Show(this, BuildContent(apps), animate: false);
        }
        catch (Exception ex) when (ex is System.IO.IOException or UnauthorizedAccessException
            or System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            AppLog.Warning($"Tray enumeration failed: {ex.Message}");
            if (cached is null && requestVersion == _requestVersion) Report(UiText.Get("SystemDataUnavailable"));
        }
    }

    /// <summary>
    /// Live tray icons hosted by <see cref="TrayHost"/>. Left and right clicks are forwarded to the app the
    /// way the native notification area sends them, so each app opens its own window or its own context menu.
    /// </summary>
    internal UIElement BuildLiveContent(IReadOnlyList<ManagedShell.WindowsTray.NotifyIcon> icons)
    {
        var root = new StackPanel { Width = 280 };
        root.Children.Add(DockFlyout.Heading(UiText.Get("TrayApps")));
        if (icons.Count == 0) root.Children.Add(DockFlyout.Caption(UiText.Get("TrayEmpty")));
        foreach (var icon in icons)
        {
            var title = icon.Title?.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries).FirstOrDefault()?.Trim();
            var name = string.IsNullOrWhiteSpace(title)
                ? (string.IsNullOrWhiteSpace(icon.Path) ? UiText.Get("TrayApps") : System.IO.Path.GetFileNameWithoutExtension(icon.Path))
                : title;
            var image = new Image { Source = icon.Icon, Width = 24, Height = 24, Margin = new Thickness(0, 0, 10, 0), VerticalAlignment = VerticalAlignment.Center };
            RenderOptions.SetBitmapScalingMode(image, BitmapScalingMode.HighQuality);
            var row = new DockPanel();
            row.Children.Add(image);
            row.Children.Add(new TextBlock { Text = name, FontSize = 12.5, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis });
            var open = DockFlyout.FlatButton(row, (_, _) => ForwardClick(icon, System.Windows.Input.MouseButton.Left), name);
            open.Padding = new Thickness(8, 7, 8, 7);
            open.PreviewMouseRightButtonDown += (_, _) => _flyout.RetainForExternalContextMenu(icon.HWnd);
            open.PreviewMouseRightButtonUp += (_, e) =>
            {
                e.Handled = true;
                ForwardClick(icon, System.Windows.Input.MouseButton.Right);
            };

            var appRow = new DockPanel { Margin = new Thickness(0, 1, 0, 1) };
            if (!string.IsNullOrEmpty(icon.Path))
            {
                var quitIcon = new ContentControl { Content = FindResource("IconClose"), Width = 14, Height = 14 };
                quitIcon.SetResourceReference(StyleProperty, "WidgetIcon");
                var path = icon.Path;
                var quit = new Button
                {
                    Content = quitIcon,
                    ToolTip = $"{name}: {UiText.Get("Terminate")}",
                    VerticalAlignment = VerticalAlignment.Center,
                    Margin = new Thickness(4, 0, 0, 0)
                };
                quit.SetResourceReference(StyleProperty, "WidgetDangerIconButton");
                AutomationProperties.SetName(quit, $"{name}: {UiText.Get("Terminate")}");
                quit.Click += (_, _) =>
                {
                    _flyout.Close();
                    TerminateApplicationRequested?.Invoke(path);
                };
                DockPanel.SetDock(quit, Dock.Right);
                appRow.Children.Add(quit);
            }
            appRow.Children.Add(open);
            root.Children.Add(appRow);
        }

        return root;
    }

    private void ForwardClick(ManagedShell.WindowsTray.NotifyIcon icon, System.Windows.Input.MouseButton button)
    {
        if (button == System.Windows.Input.MouseButton.Left)
        {
            _flyout.Close();
            // Let the flyout finish closing before the app takes the foreground for its window.
            Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Background, () => TrayHost.Click(icon, button));
        }
        else
        {
            // Right-clicks open the app's context menu; retain the flyout open.
            _flyout.RetainForExternalContextMenu(icon.HWnd);
            Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Background, () =>
            {
                TrayHost.Click(icon, button);
            });
        }
    }

    internal UIElement BuildContent(IReadOnlyList<TrayApp> apps)
    {
        var root = new StackPanel { Width = 280 };
        root.Children.Add(DockFlyout.Heading(UiText.Get("TrayApps")));
        if (apps.Count == 0) root.Children.Add(DockFlyout.Caption(UiText.Get("TrayEmpty")));
        foreach (var app in apps)
        {
            var image = new Image { Source = app.Icon, Width = 24, Height = 24, Margin = new Thickness(0, 0, 10, 0), VerticalAlignment = VerticalAlignment.Center };
            RenderOptions.SetBitmapScalingMode(image, BitmapScalingMode.HighQuality);
            var row = new DockPanel();
            row.Children.Add(image);
            row.Children.Add(new TextBlock { Text = app.Name, FontSize = 12.5, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis });
            var open = DockFlyout.FlatButton(row, (_, _) =>
            {
                _flyout.Close();
                Report(TrayIcons.Open(app));
            }, app.Name);
            open.Padding = new Thickness(8, 7, 8, 7);
            var menu = new ContextMenu();
            var closeItem = new MenuItem { Header = UiText.Get("CloseApplication"), Icon = FindResource("IconClose") };
            closeItem.Click += (_, _) =>
            {
                _flyout.Close();
                CloseApplicationRequested?.Invoke(app.ExecutablePath);
            };
            var terminateItem = new MenuItem { Header = UiText.Get("Terminate"), Icon = FindResource("IconTerminate") };
            terminateItem.SetResourceReference(StyleProperty, "DestructiveMenuItem");
            terminateItem.Click += (_, _) =>
            {
                _flyout.Close();
                TerminateApplicationRequested?.Invoke(app.ExecutablePath);
            };
            menu.Items.Add(closeItem);
            menu.Items.Add(terminateItem);
            // DockFlyout hooks ContextMenuOpening at its root and owns StaysOpen for every flyout context menu;
            // duplicating that logic here used to double-manage the same popup state.
            open.ContextMenu = menu;

            // Tray apps may keep running after WM_CLOSE; force quit uses the dock's existing confirmation flow.
            var quitIcon = new ContentControl
            {
                Content = FindResource("IconClose"), Width = 14, Height = 14
            };
            quitIcon.SetResourceReference(StyleProperty, "WidgetIcon");
            var quit = new Button
            {
                Content = quitIcon,
                ToolTip = $"{app.Name}: {UiText.Get("Terminate")}",
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(4, 0, 0, 0)
            };
            quit.SetResourceReference(StyleProperty, "WidgetDangerIconButton");
            AutomationProperties.SetName(quit, $"{app.Name}: {UiText.Get("Terminate")}");
            quit.Click += (_, _) =>
            {
                _flyout.Close();
                TerminateApplicationRequested?.Invoke(app.ExecutablePath);
            };
            var appRow = new DockPanel { Margin = new Thickness(0, 1, 0, 1) };
            DockPanel.SetDock(quit, Dock.Right);
            appRow.Children.Add(quit);
            appRow.Children.Add(open);
            root.Children.Add(appRow);
        }

        return root;
    }
}
