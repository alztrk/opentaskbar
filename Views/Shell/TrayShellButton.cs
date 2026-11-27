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
    private static readonly Func<IReadOnlyList<TrayApp>> SharedReader = static () => TrayIconSource.Shared.Snapshot;

    private readonly DockFlyout _flyout = new();
    private readonly ContentControl _glyph;
    private int _requestVersion;
    private IReadOnlyList<TrayApp>? _lastApps;
    private Task<IReadOnlyList<TrayApp>>? _refresh;
    private Action<string>? _reportError;
    private bool _snapshotSubscribed;

    /// <summary>Seam for tests: enumerates the tray apps. Defaults to the shared tray icon snapshot.</summary>
    internal Func<IReadOnlyList<TrayApp>> AppsReader { get; set; } = SharedReader;

    /// <summary>Supplies the persisted pinned tray icon keys so the flyout reflects pinned state.</summary>
    public Func<IReadOnlyList<string>?>? PinnedKeysProvider { get; set; }

    public event Action<string>? PinToggled;
    public event Action<string>? CloseApplicationRequested;
    public event Action<string>? TerminateApplicationRequested;

    public TrayShellButton()
    {
        // Same Grid + TransformGroup cell as the other shell buttons so magnification and edge-origin updates
        // can drive the existing transforms instead of allocating one on first hover.
        _glyph = new ContentControl
        {
            Content = FindResource("IconChevronUp"),
            Width = (double)Application.Current.Resources["DockUtilityGlyphSize"],
            Height = (double)Application.Current.Resources["DockUtilityGlyphSize"],
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center
        };
        _glyph.SetResourceReference(StyleProperty, "WidgetIcon");
        _glyph.SetResourceReference(ForegroundProperty, "TextPrimary");
        var cell = new Grid
        {
            Width = (double)Application.Current.Resources["DockIconSize"],
            Height = (double)Application.Current.Resources["DockIconSize"],
            RenderTransformOrigin = new Point(0.5, 0.5)
        };
        // Vertical docks can make the button shorter than DockIconSize. Keep the
        // transform cell inside the button so the glyph stays centered.
        SizeChanged += (_, _) =>
        {
            var size = Math.Min(ActualWidth, ActualHeight);
            if (size > 0)
            {
                cell.Width = size;
                cell.Height = size;
            }
        };
        cell.RenderTransform = new TransformGroup
        {
            Children =
            {
                new ScaleTransform(1, 1),
                new TranslateTransform()
            }
        };
        cell.Children.Add(_glyph);
        Content = cell;
        Click += Button_Click;
        // Warm the list while the pointer approaches, so a click can show it at once.
        MouseEnter += (_, _) => _ = RefreshAppsQuietlyAsync();
        Loaded += (_, _) =>
        {
            SubscribeSharedSnapshot();
            UiText.Current.PropertyChanged -= UiText_PropertyChanged;
            UiText.Current.PropertyChanged += UiText_PropertyChanged;
        };
        Unloaded += (_, _) =>
        {
            UiText.Current.PropertyChanged -= UiText_PropertyChanged;
            TrayIconSource.Shared.SnapshotChanged -= OnSharedSnapshotChanged;
            _snapshotSubscribed = false;
            _flyout.Close();
        };
        SubscribeSharedSnapshot();
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        Localize();
    }

    private void SubscribeSharedSnapshot()
    {
        if (_snapshotSubscribed) return;
        _snapshotSubscribed = true;
        TrayIconSource.Shared.SnapshotChanged += OnSharedSnapshotChanged;
    }

    /// <summary>Adopts the shared snapshot on the UI thread so the flyout never shows a stale private cache.</summary>
    private void OnSharedSnapshotChanged()
    {
        if (!Dispatcher.CheckAccess())
        {
            if (!Dispatcher.HasShutdownStarted && !Dispatcher.HasShutdownFinished)
                _ = Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Background, OnSharedSnapshotChanged);
            return;
        }
        if (!ReferenceEquals(AppsReader, SharedReader)) return;
        _lastApps = TrayIconSource.Shared.Snapshot;
        if (_flyout.IsOpen && TrayHost.Current is not { IsRunning: true })
        {
            _flyout.Show(this, BuildContent(_lastApps ?? []), animate: false);
        }
    }

    private void UiText_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (Dispatcher.HasShutdownStarted || Dispatcher.HasShutdownFinished) return;
        if (!Dispatcher.CheckAccess())
        {
            _ = Dispatcher.BeginInvoke(() => UiText_PropertyChanged(sender, e));
            return;
        }
        Localize();
        if (_flyout.IsOpen)
        {
            if (TrayHost.Current is { IsRunning: true } host)
                _flyout.Show(this, BuildLiveContent(host.AppIcons()), animate: false);
            else if (_lastApps is not null)
                _flyout.Show(this, BuildContent(_lastApps ?? []), animate: false);
        }
    }

    /// <summary>Points the chevron toward the direction the background-apps flyout opens.</summary>
    public void SetEdge(OpenTaskbar.Models.DockEdge edge)
    {
        var key = edge switch
        {
            OpenTaskbar.Models.DockEdge.Left => "IconChevronRight",
            OpenTaskbar.Models.DockEdge.Right => "IconChevronLeft",
            _ => "IconChevronUp"
        };
        _glyph.Content = TryFindResource(key) ?? FindResource("IconChevronUp");
    }

    /// <summary>Wires the dock's error reporter and process actions. Called once from MainWindow.</summary>
    public void Initialize(Action<string> reportError,
        Action<string> closeApplication, Action<string> terminateApplication)
    {
        _reportError = reportError;
        CloseApplicationRequested += closeApplication;
        TerminateApplicationRequested += terminateApplication;
    }

    private void Report(string? message)
    {
        if (!string.IsNullOrEmpty(message)) _reportError?.Invoke(message);
    }

    /// <summary>
    /// One enumeration at a time. The default reader is the shared <see cref="TrayIconSource"/>; a test seam reader
    /// still runs off the UI thread with cancellation. The await continues on the dispatcher that started it.
    /// </summary>
    private Task<IReadOnlyList<TrayApp>> RefreshAppsAsync(CancellationToken cancellationToken = default)
    {
        if (_refresh is { IsCompleted: false } running) return running;
        if (ReferenceEquals(AppsReader, SharedReader))
        {
            return _refresh = TrayIconSource.Shared.RefreshAsync(cancellationToken);
        }
        var reader = AppsReader;
        return _refresh = Task.Run(reader, cancellationToken);
    }

    private async Task RefreshAppsQuietlyAsync()
    {
        try
        {
            await RefreshAppsAsync().ConfigureAwait(true);
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
        var root = new StackPanel { Width = 296 };
        root.Children.Add(DockFlyout.Heading(UiText.Get("TrayApps")));
        if (icons.Count == 0) root.Children.Add(DockFlyout.Caption(UiText.Get("TrayEmpty")));
        var pinned = PinnedKeysProvider?.Invoke();
        foreach (var icon in icons)
        {
            var name = TrayPinIdentity.CleanTitle(icon.Title, icon.Path, UiText.Get("TrayApps"));
            var image = new Image { Source = icon.Icon, Width = 24, Height = 24, Margin = new Thickness(0, 0, 10, 0), VerticalAlignment = VerticalAlignment.Center };
            RenderOptions.SetBitmapScalingMode(image, BitmapScalingMode.HighQuality);
            var row = new DockPanel();
            row.Children.Add(image);
            row.Children.Add(new TextBlock { Text = name, FontSize = 12.5, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis });
            var canonicalKey = TrayPinIdentity.KeyFor(icon.GUID, icon.Path, icon.UID);
            var matchedKey = TrayPinIdentity.FindMatchingKey(pinned, icon.GUID, icon.Path, icon.UID);
            var toggleKey = matchedKey ?? canonicalKey;

            var open = DockFlyout.FlatButton(row, (_, _) => ForwardClick(icon, System.Windows.Input.MouseButton.Left), name);
            open.Padding = new Thickness(8, 7, 8, 7);
            open.PreviewMouseRightButtonDown += (_, _) =>
            {
                if (TrayHost.CanForwardInputToWindow(icon.HWnd))
                {
                    open.ContextMenu = null;
                    _flyout.RetainForExternalContextMenu(icon.HWnd);
                }
                else
                {
                    open.ContextMenu = BuildLiveFallbackMenu(name, icon, toggleKey, matchedKey is not null, () =>
                    {
                        var currentIcons = TrayHost.Current is { IsRunning: true } host ? host.AppIcons() : icons;
                        _flyout.Show(this, BuildLiveContent(currentIcons), animate: false);
                    });
                }
            };
            open.PreviewMouseRightButtonUp += (_, e) =>
            {
                if (!TrayHost.CanForwardInputToWindow(icon.HWnd)) return;
                e.Handled = true;
                ForwardClick(icon, System.Windows.Input.MouseButton.Right, () =>
                {
                    var menu = open.ContextMenu ??= BuildLiveFallbackMenu(name, icon, toggleKey, matchedKey is not null, () =>
                    {
                        var currentIcons = TrayHost.Current is { IsRunning: true } host ? host.AppIcons() : icons;
                        _flyout.Show(this, BuildLiveContent(currentIcons), animate: false);
                    });
                    menu.PlacementTarget = open;
                    menu.Placement = System.Windows.Controls.Primitives.PlacementMode.MousePoint;
                    menu.IsOpen = true;
                });
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

            if (!string.IsNullOrEmpty(toggleKey))
            {
                var pinButton = CreatePinButton(name, toggleKey, matchedKey is not null, () =>
                {
                    var currentIcons = TrayHost.Current is { IsRunning: true } host ? host.AppIcons() : icons;
                    _flyout.Show(this, BuildLiveContent(currentIcons), animate: false);
                });
                DockPanel.SetDock(pinButton, Dock.Right);
                appRow.Children.Add(pinButton);
            }

            appRow.Children.Add(open);
            root.Children.Add(appRow);
        }

        return root;
    }

    private ContextMenu BuildLiveFallbackMenu(
        string name,
        ManagedShell.WindowsTray.NotifyIcon icon,
        string toggleKey,
        bool isPinned,
        Action refreshFlyout)
    {
        var menu = new ContextMenu();
        var path = icon.Path?.Trim();

        if (!string.IsNullOrEmpty(path))
        {
            var openItem = new MenuItem { Header = UiText.Get("OpenApplication") };
            openItem.Click += (_, _) =>
            {
                _flyout.Close();
                Report(TrayIcons.Open(new TrayApp(name, path, icon.Icon as System.Windows.Media.Imaging.BitmapSource,
                    icon.GUID == Guid.Empty ? null : icon.GUID, icon.UID, IconSource.Live)));
            };
            menu.Items.Add(openItem);
        }

        if (!string.IsNullOrEmpty(toggleKey))
        {
            var pinItem = new MenuItem
            {
                Header = UiText.Get(isPinned ? "UnpinTrayIcon" : "PinTrayIcon"),
                Icon = FindResource("IconPin")
            };
            pinItem.Click += (_, _) =>
            {
                var wasOpen = _flyout.IsOpen;
                PinToggled?.Invoke(toggleKey);
                if (wasOpen) refreshFlyout();
            };
            menu.Items.Add(pinItem);
        }

        if (!string.IsNullOrEmpty(path))
        {
            var closeItem = new MenuItem { Header = UiText.Get("CloseApplication"), Icon = FindResource("IconClose") };
            closeItem.Click += (_, _) =>
            {
                _flyout.Close();
                CloseApplicationRequested?.Invoke(path);
            };
            var terminateItem = new MenuItem { Header = UiText.Get("Terminate"), Icon = FindResource("IconTerminate") };
            terminateItem.SetResourceReference(StyleProperty, "DestructiveMenuItem");
            terminateItem.Click += (_, _) =>
            {
                _flyout.Close();
                TerminateApplicationRequested?.Invoke(path);
            };
            menu.Items.Add(new Separator());
            menu.Items.Add(closeItem);
            menu.Items.Add(terminateItem);
        }

        return menu;
    }

    private Button CreatePinButton(string name, string pinKey, bool isPinned, Action refreshFlyout)
    {
        var actionText = UiText.Get(isPinned ? "UnpinTrayIcon" : "PinTrayIcon");
        var label = $"{name}: {actionText}";
        var pinIcon = new ContentControl
        {
            Content = FindResource("IconPin"),
            Width = 14,
            Height = 14
        };
        pinIcon.SetResourceReference(StyleProperty, "WidgetIcon");

        var pinButton = new Button
        {
            Content = pinIcon,
            Width = 29,
            Height = 29,
            ToolTip = label,
            Tag = pinKey,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(4, 0, 0, 0)
        };
        pinButton.SetResourceReference(StyleProperty, "WidgetIconButton");
        pinButton.SetResourceReference(ForegroundProperty, isPinned ? "AccentBrush" : "TextSecondary");
        if (isPinned)
        {
            pinButton.SetResourceReference(BackgroundProperty, "WidgetSelectedFill");
        }
        AutomationProperties.SetName(pinButton, label);
        pinButton.Click += (_, _) =>
        {
            var wasOpen = _flyout.IsOpen;
            PinToggled?.Invoke(pinKey);
            if (wasOpen) refreshFlyout();
        };
        return pinButton;
    }

    private void ForwardClick(
        ManagedShell.WindowsTray.NotifyIcon icon,
        System.Windows.Input.MouseButton button,
        Action? onForwardFailure = null)
    {
        if (button == System.Windows.Input.MouseButton.Left)
        {
            _flyout.Close();
            // Let the flyout finish closing before the app takes the foreground for its window.
            Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Background, () =>
            {
                if (TrayHost.TryClick(icon, button)) return;
                OpenLiveIcon(icon);
            });
        }
        else
        {
            // Right-clicks open the app's context menu; retain the flyout open.
            _flyout.RetainForExternalContextMenu(icon.HWnd);
            Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Background, () =>
            {
                if (!TrayHost.TryClick(icon, button)) onForwardFailure?.Invoke();
            });
        }
    }

    private void OpenLiveIcon(ManagedShell.WindowsTray.NotifyIcon icon)
    {
        var path = icon.Path?.Trim();
        if (string.IsNullOrEmpty(path)) return;

        var name = TrayPinIdentity.CleanTitle(icon.Title, path, UiText.Get("TrayApps"));
        Report(TrayIcons.Open(new TrayApp(name, path, icon.Icon as System.Windows.Media.Imaging.BitmapSource,
            icon.GUID == Guid.Empty ? null : icon.GUID, icon.UID, IconSource.Live)));
    }

    internal UIElement BuildContent(IReadOnlyList<TrayApp> apps)
    {
        var root = new StackPanel { Width = 296 };
        root.Children.Add(DockFlyout.Heading(UiText.Get("TrayApps")));
        if (apps.Count == 0) root.Children.Add(DockFlyout.Caption(UiText.Get("TrayEmpty")));
        var pinned = PinnedKeysProvider?.Invoke();
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
                Content = FindResource("IconClose"),
                Width = 14,
                Height = 14
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

            var canonicalKey = TrayPinIdentity.KeyForPath(app.ExecutablePath);
            var matchedKey = TrayPinIdentity.FindMatchingPathKey(pinned, app.ExecutablePath);
            var toggleKey = matchedKey ?? canonicalKey;
            if (!string.IsNullOrEmpty(toggleKey))
            {
                var pinButton = CreatePinButton(app.Name, toggleKey, matchedKey is not null, () =>
                {
                    _flyout.Show(this, BuildContent(_lastApps ?? apps), animate: false);
                });
                DockPanel.SetDock(pinButton, Dock.Right);
                appRow.Children.Add(pinButton);
            }

            appRow.Children.Add(open);
            root.Children.Add(appRow);
        }

        return root;
    }
}
