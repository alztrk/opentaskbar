using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>Stands in for the hidden notification area: lists running background apps that own a tray icon.</summary>
public sealed class TrayWidget : DockWidget
{
    private readonly Button _button;
    private readonly DockFlyout _flyout = new();
    private int _requestVersion;
    private IReadOnlyList<TrayApp>? _lastApps;
    private Task<IReadOnlyList<TrayApp>>? _refresh;

    /// <summary>Seam for tests: enumerates the tray apps (registry snapshot plus every running process).</summary>
    internal Func<IReadOnlyList<TrayApp>> AppsReader { get; set; } = TrayIcons.RunningApps;

    public event Action<string>? CloseApplicationRequested;
    public event Action<string>? TerminateApplicationRequested;

    public TrayWidget()
    {
        var glyph = new ContentControl { Content = FindResource("IconChevronUp"), Width = 16, Height = 16 };
        glyph.SetResourceReference(StyleProperty, "WidgetIcon");
        glyph.SetResourceReference(ForegroundProperty, "TextPrimary");
        _button = new Button { Content = glyph, Padding = new Thickness(10, 0, 10, 0) };
        _button.SetResourceReference(StyleProperty, "WidgetCardButton");
        _button.Click += Button_Click;
        // Warm the list while the pointer approaches, so a click can show it at once.
        _button.MouseEnter += (_, _) => RefreshAppsQuietly();
        Localize();
        Content = _button;
    }

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        Localize();
        RefreshAppsQuietly();
    }

    /// <summary>
    /// One enumeration at a time, shared by every caller. It reads the tray registry snapshot and every running
    /// process (~100 ms), so it always runs off the UI thread and its result is kept for the next click.
    /// </summary>
    private Task<IReadOnlyList<TrayApp>> RefreshAppsAsync()
    {
        if (_refresh is { IsCompleted: false } running) return running;
        var reader = AppsReader;
        return _refresh = LoadAsync();

        async Task<IReadOnlyList<TrayApp>> LoadAsync()
        {
            var apps = await Task.Run(reader);
            _lastApps = apps;
            return apps;
        }
    }

    private async void RefreshAppsQuietly()
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

    protected override void OnDeactivated()
    {
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        _requestVersion++;
        _flyout.Close();
    }

    private void UiText_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e) => Localize();


    private void Localize()
    {
        _button.ToolTip = UiText.Get("TrayApps");
        System.Windows.Automation.AutomationProperties.SetName(_button, UiText.Get("TrayApps"));
    }

    private async void Button_Click(object sender, RoutedEventArgs e)
    {
        if (_flyout.IsOpen || _flyout.RecentlyClosed(_button))
        {
            _flyout.Close();
            return;
        }

        var requestVersion = ++_requestVersion;
        if (TrayHost.Current is { IsRunning: true } host)
        {
            _flyout.Show(_button, BuildLiveContent(host.AppIcons()));
            return;
        }

        // Show the last known list at once instead of waiting for a fresh enumeration; update it in place if the
        // fresh list differs. Only the very first open, with nothing cached yet, waits for the enumeration.
        var cached = _lastApps;
        if (cached is not null) _flyout.Show(_button, BuildContent(cached));
        try
        {
            var apps = await RefreshAppsAsync();
            if (!IsActive || requestVersion != _requestVersion) return;
            if (cached is null) _flyout.Show(_button, BuildContent(apps));
            else if (_flyout.IsOpen && !SameApps(cached, apps)) _flyout.Show(_button, BuildContent(apps), animate: false);
        }
        catch (Exception ex) when (ex is System.IO.IOException or UnauthorizedAccessException
            or System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            AppLog.Warning($"Tray enumeration failed: {ex.Message}");
            if (cached is null && IsActive && requestVersion == _requestVersion) Report(UiText.Get("SystemDataUnavailable"));
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
            var image = new Image { Source = icon.Icon, Width = 20, Height = 20, Margin = new Thickness(0, 0, 10, 0) };
            RenderOptions.SetBitmapScalingMode(image, BitmapScalingMode.HighQuality);
            var row = new DockPanel();
            row.Children.Add(image);
            row.Children.Add(new TextBlock { Text = name, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis });
            var open = DockFlyout.FlatButton(row, (_, _) => ForwardClick(icon, System.Windows.Input.MouseButton.Left), name);
            // The app draws its own menu; close the flyout first so the menu keeps the foreground.
            open.PreviewMouseRightButtonUp += (_, e) =>
            {
                e.Handled = true;
                ForwardClick(icon, System.Windows.Input.MouseButton.Right);
            };

            var appRow = new DockPanel();
            if (!string.IsNullOrEmpty(icon.Path))
            {
                var quitIcon = new ContentControl { Content = FindResource("IconTerminate"), Width = 16, Height = 16 };
                quitIcon.SetResourceReference(StyleProperty, "WidgetIcon");
                quitIcon.SetResourceReference(ForegroundProperty, "TextPrimary");
                var path = icon.Path;
                var quit = DockFlyout.FlatButton(quitIcon, (_, _) =>
                {
                    _flyout.Close();
                    TerminateApplicationRequested?.Invoke(path);
                }, $"{name}: {UiText.Get("Terminate")}");
                DockPanel.SetDock(quit, Dock.Right);
                appRow.Children.Add(quit);
            }
            appRow.Children.Add(open);
            root.Children.Add(appRow);
        }

        var settings = DockFlyout.FlatButton(new TextBlock { Text = UiText.Get("TraySettings"), FontWeight = FontWeights.SemiBold }, (_, _) =>
        {
            _flyout.Close();
            Report(ShellShortcuts.OpenUri("ms-settings:taskbar"));
        });
        settings.Margin = new Thickness(0, 6, 0, 0);
        root.Children.Add(settings);
        return root;
    }

    private void ForwardClick(ManagedShell.WindowsTray.NotifyIcon icon, System.Windows.Input.MouseButton button)
    {
        _flyout.Close();
        // Let the flyout finish closing before the app takes the foreground for its window or menu.
        Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Background, () => TrayHost.Click(icon, button));
    }

    internal UIElement BuildContent(IReadOnlyList<TrayApp> apps)
    {
        var root = new StackPanel { Width = 280 };
        root.Children.Add(DockFlyout.Heading(UiText.Get("TrayApps")));
        if (apps.Count == 0) root.Children.Add(DockFlyout.Caption(UiText.Get("TrayEmpty")));
        foreach (var app in apps)
        {
            var image = new Image { Source = app.Icon, Width = 20, Height = 20, Margin = new Thickness(0, 0, 10, 0) };
            RenderOptions.SetBitmapScalingMode(image, BitmapScalingMode.HighQuality);
            var row = new DockPanel();
            row.Children.Add(image);
            row.Children.Add(new TextBlock { Text = app.Name, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis });
            var open = DockFlyout.FlatButton(row, (_, _) =>
            {
                _flyout.Close();
                Report(TrayIcons.Open(app));
            }, app.Name);
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
            open.ContextMenu = menu;

            // Tray apps may keep running after WM_CLOSE; force quit uses the dock's existing confirmation flow.
            var quitIcon = new ContentControl
            {
                Content = FindResource("IconTerminate"), Width = 16, Height = 16
            };
            quitIcon.SetResourceReference(StyleProperty, "WidgetIcon");
            quitIcon.SetResourceReference(ForegroundProperty, "TextPrimary");
            var quit = DockFlyout.FlatButton(quitIcon, (_, _) =>
            {
                _flyout.Close();
                TerminateApplicationRequested?.Invoke(app.ExecutablePath);
            }, $"{app.Name}: {UiText.Get("Terminate")}");
            var appRow = new DockPanel();
            DockPanel.SetDock(quit, Dock.Right);
            appRow.Children.Add(quit);
            appRow.Children.Add(open);
            root.Children.Add(appRow);
        }

        var settings = DockFlyout.FlatButton(new TextBlock { Text = UiText.Get("TraySettings"), FontWeight = FontWeights.SemiBold }, (_, _) =>
        {
            _flyout.Close();
            Report(ShellShortcuts.OpenUri("ms-settings:taskbar"));
        });
        settings.Margin = new Thickness(0, 6, 0, 0);
        root.Children.Add(settings);
        root.Children.Add(DockFlyout.Caption(UiText.Get("TrayNote")));
        return root;
    }
}
