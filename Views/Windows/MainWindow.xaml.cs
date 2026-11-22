using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Widgets;
using OpenTaskbar.Widgets;

namespace OpenTaskbar;

public partial class MainWindow : Window
{
    private const int GwlExStyle = -20;
    private const long WsExAppWindow = 0x00040000;
    private const long WsExToolWindow = 0x00000080;
    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoMove = 0x0002;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;
    private const uint SwpFrameChanged = 0x0020;
    private const int WmMouseActivate = 0x0021;
    private const int MaNoActivate = 3;
    private const int WmNcHitTest = 0x0084;
    private const int HtTransparent = -1;

    private readonly ObservableCollection<DockApp> _pinnedApps = [];
    private readonly ObservableCollection<DockApp> _runningApps = [];
    private readonly ObservableCollection<DockWidgetItem> _widgets = [];
    private readonly DockWidgetService _widgetService = new(TimeProvider.System);
    private readonly WidgetPanelHost _widgetPanel;
    private readonly DispatcherTimer _timedWidgetRefreshTimer;
    private readonly HashSet<string> _exitingRunningPaths = new(StringComparer.OrdinalIgnoreCase);
    private readonly ApplicationPreferences _preferences = ((App)Application.Current).Preferences;
    private SettingsWindow? _settingsWindow;
    private readonly ApplicationJumpListService _jumpLists = new();
    private CancellationTokenSource? _jumpListLoad;
    private long? _jumpListSession;
    private readonly DispatcherTimer _previewHoverTimer;
    private readonly DispatcherTimer _previewCloseTimer;
    private Button? _pendingPreviewButton;
    private readonly DispatcherTimer _widgetRefreshTimer;
    private readonly DispatcherTimer _iconRetryTimer;
    private readonly RunningAppIconCache _runningAppIcons = new();
    private readonly WindowThumbnailPreview _thumbnailPreview;
    private readonly Dictionary<string, nint> _lastSelectedWindows = new(StringComparer.OrdinalIgnoreCase);
    private DockPosition _dockPosition = DockPosition.Left;
    private NativeWindowTracker? _windowTracker;
    private TaskbarReplacementSession? _replacementSession;
    private HwndSource? _source;
    private nint _foregroundWindow;
    private readonly DispatcherTimer _foregroundPollTimer;
    private readonly List<FrameworkElement> _magnifiedIcons = [];
    private ContextMenu? _openContextMenu;
    private OpenTaskbar.Widgets.DockWidget? _contextTargetWidget;
    private FrameworkElement? _contextTargetElement;
    private bool _contextTargetWasForeground;
    private nint _foregroundAtMenuOpen;
    private DateTime _contextMenuOpenedAtUtc;
    private Size _dockArea;
    private nint? _foregroundAtAppPress;
    private readonly StartMenuState _startMenu = new();
    private bool _startMenuOpenAtPress;
    private nint _lastApplicationWindow;
    private uint _taskbarCreatedMessage;
    private Point _dragStartPoint;
    private readonly HashSet<FrameworkElement> _displacedIcons = [];
    private DockApp? _draggedApp;
    private DockAutoHideController? _autoHideController;
    private double? _dockWidthOverride;
    private (DockSize Size, double Scale, int Padding, double BottomGap, double TopGap)? _appliedDockGeometry;
    private bool _resizingDock;
    private bool _resizePending;
    private bool _resizingFromLeft;
    private readonly bool _isInspection = ((App)Application.Current).IsInspection;
    // Failures are written to the log only; the dock never displays error popups.
    private readonly WidgetCoordinator _widgetCoordinator;
    private readonly Dictionary<string, FrameworkElement> _dockItemElements = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, ContentControl> _legacyWidgetElements = new(StringComparer.OrdinalIgnoreCase);
    private Point _legacyDragStartPoint;
    private ContentControl? _draggedLegacyWidget;
    private bool _rebuildingDockOrder;
    private bool _instantDockResize;
    private double _dockTopGap;
    private (DockSize Size, double Scale, int Padding)? _sizePreviewOverride;
    private double? _radiusPreviewOverride;
    private AudioEndpointMonitor.SharedHandle? _wheelAudio;
    private int _wheelRemainder;

    static MainWindow()
    {
        EventManager.RegisterClassHandler(
            typeof(System.Windows.Controls.ToolTip),
            System.Windows.Controls.ToolTip.OpenedEvent,
            new RoutedEventHandler(OnToolTipOpened));
    }

    private static void OnToolTipOpened(object sender, RoutedEventArgs e)
    {
        if (sender is not System.Windows.Controls.ToolTip tooltip) return;

        void Elevate()
        {
            if (PresentationSource.FromVisual(tooltip) is HwndSource source && source.Handle != nint.Zero)
            {
                SetWindowPos(source.Handle, new nint(-1), 0, 0, 0, 0, SwpNoSize | SwpNoMove | SwpNoActivate);
            }
        }

        Elevate();
        tooltip.Dispatcher.BeginInvoke((Action)Elevate, DispatcherPriority.Loaded);
    }

    public MainWindow()
    {
        var startup = StartupStopwatch.Start("MainWindow constructor");
        InitializeComponent();
        StartupStopwatch.Mark("XAML inflated");
        // LayoutUpdated fires on every Arrange (including fisheye's per-frame slot writes). Rebuilding
        // rest geometry from it created a feedback loop; subscribe to the concrete geometry changes instead.
        DockContents.SizeChanged += (_, _) => InvalidateMagnificationTargets();
        DockScrollViewer.ScrollChanged += (_, _) => InvalidateMagnificationTargets();
        _widgetPanel = new WidgetPanelHost(this);
        _widgetPanel.IsOpenChanged += WidgetPanel_IsOpenChanged;
        _widgetPanel.OperationFailed += ReportError;
        ShowInTaskbar = _isInspection;
        if (((App)Application.Current).OpenSettingsOnStartup)
        {
            RoutedEventHandler? openSettingsOnce = null;
            openSettingsOnce = (_, _) =>
            {
                Loaded -= openSettingsOnce;
                OpenSettings_Click(this, new RoutedEventArgs());
            };
            Loaded += openSettingsOnce;
        }
        PinnedApps.ItemsSource = _pinnedApps;
        RunningApps.ItemsSource = _runningApps;
        AddHandler(TimedWidgetTile.DetailsRequestedEvent, new RoutedEventHandler(TimedWidget_DetailsRequested));
        _thumbnailPreview = new WindowThumbnailPreview((Style)FindResource("PreviewCloseButton"));
        _previewHoverTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher);
        _previewHoverTimer.Tick += PreviewHoverTimer_Tick;
        _previewCloseTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(500)
        };
        _previewCloseTimer.Tick += PreviewCloseTimer_Tick;
        // Non-interactive refresh work runs at idle priority so it never competes with rendering.
        _widgetRefreshTimer = new DispatcherTimer(DispatcherPriority.ApplicationIdle, Dispatcher)
        {
            Interval = TimeSpan.FromSeconds(15)
        };
        _widgetRefreshTimer.Tick += WidgetRefreshTimer_Tick;
        _iconRetryTimer = new DispatcherTimer(DispatcherPriority.ApplicationIdle, Dispatcher)
        {
            Interval = RunningAppIconCache.RetryDelay
        };
        _iconRetryTimer.Tick += IconRetryTimer_Tick;
        // Icons load off the UI thread; refresh the running apps once when one or more loads finish.
        _runningAppIcons.IconLoaded += QueueIconRefresh;
        _timedWidgetRefreshTimer = new DispatcherTimer(DispatcherPriority.ApplicationIdle, Dispatcher)
        {
            Interval = TimeSpan.FromSeconds(1)
        };
        _timedWidgetRefreshTimer.Tick += TimedWidgetRefreshTimer_Tick;
        // WinEvents are not raised for every focus transition (minimize, alt-tab,
        // desktop switches). Poll lightly so indicator dots never stay stale.
        _foregroundPollTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(400)
        };
        _foregroundPollTimer.Tick += ForegroundPollTimer_Tick;
        _widgetCoordinator = new WidgetCoordinator(
            WidgetStrip, Dispatcher, ReportError, lazy: true,
            canExpandInline: CanExpandWidgetInline,
            onHoverLayoutChanged: _ => ResizeDockToContent())
        {
            OrderChanged = OnWidgetOrderChanged
        };
        _widgetCoordinator.WidgetRemoveRequested += RemoveDockWidget;
        TrayButton.Initialize(ReportError, CloseApplication, TerminateApplication);
        _widgetCoordinator.ConfigureWidgetMenu = menu =>
        {
            menu.Opened += DockContextMenu_Opened;
            menu.Closed += DockContextMenu_Closed;
        };
        _widgetCoordinator.CreateWidgetContextMenu = _ => (ContextMenu)FindResource("DockContextMenu");
        _widgetCoordinator.HoverActiveChanged += _ => _autoHideController?.EvaluateState();
        _widgetCoordinator.ExternalLayout = true;
        _widgetCoordinator.WidgetsPlaced += () =>
        {
            ApplyStartPlacement();
            RebuildDockOrder();
        };
        Deactivated += (_, _) => _widgetCoordinator.CollapseHover();
        Activated += (_, _) => _startMenu.OnDockActivated();
        _startMenu.StartMenuOpened += hwnd =>
        {
            Dispatcher.BeginInvoke(DispatcherPriority.Normal, () =>
            {
                if (StartButton.IsVisible)
                {
                    ShellShortcuts.ScheduleStartMenuReposition(StartButton, hwnd);
                }
            });
        };
        _widgetCoordinator.AvailabilityChanged += () =>
        {
            ResizeDockToContent();
            _replacementSession?.RefreshPosition();
        };
        ApplyWidgets(_preferences.Current.Widgets);
        StartupStopwatch.Mark("widgets created");
        _thumbnailPreview.PointerEntered += ThumbnailPreview_PointerEntered;
        _thumbnailPreview.PointerLeft += ThumbnailPreview_PointerLeft;
        _thumbnailPreview.PreviewClicked += ThumbnailPreview_PreviewClicked;
        _thumbnailPreview.CloseRequested += ThumbnailPreview_CloseRequested;
        _dockPosition = _preferences.Current.Position;
        _thumbnailPreview.AnimationsEnabled = _preferences.Current.EffectiveAnimationsEnabled;
        _preferences.Changed += Preferences_Changed;
        ShellShortcuts.IsMediaWidgetActive = () =>
            _preferences.Current.Widgets?.Contains("Media", StringComparer.OrdinalIgnoreCase) == true
            || _preferences.Current.ShowMedia;
        // App.OnStartup has already selected the theme pack; later changes come through ApplicationPreferences.Save.
        ThemePacks.Changed += ThemePacks_Changed;
        Closed += (_, _) => ThemePacks.Changed -= ThemePacks_Changed;
        UpdatePreferredIconPixels();
        // Pinned apps, the start logo and shell thumbnails are populated after the first paint by
        // PopulateDockContent; none of their COM/file work belongs on the first-paint critical path.
        if (_preferences.LoadError is not null)
        {
            ReportError(_preferences.LoadError);
        }

        var startupTrimTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher)
        {
            Interval = TimeSpan.FromSeconds(3)
        };
        startupTrimTimer.Tick += (_, _) =>
        {
            startupTrimTimer.Stop();
            NativeMemoryMethods.TrimWorkingSet(force: true);
        };
        startupTrimTimer.Start();

        // Widget placement (~180 ms) is deferred with the rest of the dock content so the frame paints first.
        DockScrollViewer.SetExternalIndicators(ScrollPrevOverlay, ScrollNextOverlay);
        ApplySizeProfile();
        ApplyLayoutMode();
        ApplyDockWidthMode();
        PositionDock();
        NotificationService.UpdateRequested += OnUpdateNotificationRequested;
        startup.Finish("constructor total");
        Closed += (_, _) => NotificationService.UpdateRequested -= OnUpdateNotificationRequested;
    }

    private void OnUpdateNotificationRequested()
    {
        Dispatcher.InvokeAsync(async () =>
        {
            if (_availableUpdate is { } release)
            {
                await StartUpdateAsync(release);
            }
        });
    }

    private void Window_SourceInitialized(object? sender, EventArgs e)
    {
        var source = (HwndSource?)PresentationSource.FromVisual(this);
        if (source is null)
        {
            ReportError("ErrorDockAttach");
            return;
        }

        _source = source;
        _source.AddHook(WindowProc);
        UpdatePreferredIconPixels();
        _taskbarCreatedMessage = RegisterWindowMessage("TaskbarCreated");
        if (!_isInspection && !ExcludeFromTaskSwitcher(source.Handle))
        {
            ReportError("ErrorAltTabExclude");
        }
        WindowThumbnailPreview.ExcludeFromPeek(source.Handle);

        _windowTracker = new NativeWindowTracker(Dispatcher);
        _windowTracker.WindowsChanged += WindowTracker_WindowsChanged;
        _windowTracker.ForegroundWindowChanged += WindowTracker_ForegroundWindowChanged;
        // Window enumeration, the native taskbar takeover and feature setup are deferred to
        // Window_ContentRenderedOnce. Running them here would block WPF inside Show() before the
        // first paint, making the dock appear seconds after the executable starts.

        _autoHideController = new DockAutoHideController(
            this,
            source.Handle,
            DockSurfaceTranslate,
            DockSurface,
            () => _widgetPanel.IsOpen || (_openContextMenu?.IsOpen == true) || _thumbnailPreview.IsVisible
                || DockScrollViewer.IsPanning || LeftResizeThumb.IsDragging || RightResizeThumb.IsDragging
                || _draggedApp is not null
                || (_settingsWindow is { IsVisible: true, IsActive: true, WindowState: not WindowState.Minimized })
                || DockFlyout.IsAnyOpen || _keyboardNavigationActive);
        _autoHideController.DockBoundsProvider = GetDockPhysicalBounds;
        _autoHideController.IsShellMenuOpen = _startMenu.IsStartMenuOpenNow;
        // The Start menu and the Alt+Tab switcher cover the dock but are not applications: look past them.
        _autoHideController.IsShellMenuWindow = IsShellOverlayWindow;
        _autoHideController.LastApplicationWindowProvider = () => _lastApplicationWindow;
        _autoHideController.BackgroundWindowsProvider = () => (_windowTracker?.Snapshot ?? []).Select(window => window.Handle);
        _startMenu.StartWatching();
        _autoHideController.AnimationsEnabled = _preferences.Current.EffectiveAnimationsEnabled;
        _autoHideController.SetMode(_preferences.Current.AutoHide);
        ApplyBackdropHitTesting();

        // The expensive startup work runs once the window has actually been rendered, so the dock
        // shows up immediately instead of appearing only after all shell/COM/process calls finish.
        ContentRendered += Window_ContentRenderedOnce;
#if DEBUG
        // Verifies crash recovery: the unhandled-exception handlers must restore the Windows taskbar.
        if (Environment.GetCommandLineArgs().Contains("--crash-test"))
        {
            var crashTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(4) };
            crashTimer.Tick += (_, _) => throw new InvalidOperationException("Crash test requested with --crash-test.");
            crashTimer.Start();
        }

        // Opens Settings at startup so its XAML and bindings can be exercised without clicking the dock.
        if (Environment.GetCommandLineArgs().Contains("--open-settings"))
        {
            Dispatcher.BeginInvoke(DispatcherPriority.ApplicationIdle, () => OpenSettings_Click(this, new RoutedEventArgs()));
        }
#endif
    }

    private bool _startupFinalized;

    /// <summary>
    /// Runs on the first render, after the dock is already on screen. Everything here used to run
    /// inside SourceInitialized (i.e. inside Show()), so Explorer shell calls, the watchdog process
    /// launch and ManagedShell's notification-area init delayed the window's first paint by seconds.
    /// </summary>
    private void Window_ContentRenderedOnce(object? sender, EventArgs e)
    {
        ContentRendered -= Window_ContentRenderedOnce;
        if (_startupFinalized) return;
        _startupFinalized = true;

        StartupStopwatch.Mark("dock first paint");
        // Loaded priority sits below Render but above Input: both phases run right after the first
        // paint, cannot be starved by mouse movement, and the handler itself returns immediately.
        Dispatcher.BeginInvoke(DispatcherPriority.Loaded, PopulateDockContent);
        Dispatcher.BeginInvoke(DispatcherPriority.Loaded, FinalizeStartup);
    }

    /// <summary>Fills the already-visible dock with pins, the start logo and extras.</summary>
    private void PopulateDockContent()
    {
        if (!IsLoaded) return;
        var startup = StartupStopwatch.Start("dock content");
        _widgetCoordinator.Apply(_preferences.Current);
        StartupStopwatch.Mark("widgets placed");
        ApplyStartIcon();
        StartupStopwatch.Mark("start icon applied");
        LoadPinnedApps(_preferences.Current.PinOrder);
        StartupStopwatch.Mark("pinned apps and icons loaded");
        InitializeExtras();
        RebuildDockOrder();
        PositionDock();
        startup.Finish("content total");
    }

    /// <summary>Native taskbar replacement, window tracking and feature wiring, after the dock is visible.</summary>
    private void FinalizeStartup()
    {
        if (!IsLoaded) return;
        var startup = StartupStopwatch.Start("startup finalize");
        if (_windowTracker is not null)
        {
            try
            {
                _windowTracker.Start(_source!.Handle);
                if (!_foregroundPollTimer.IsEnabled) _foregroundPollTimer.Start();
                if (!_windowTracker.NotificationsAvailable)
                {
                    ReportError("ErrorNotificationBadge");
                }
            }
            catch (InvalidOperationException ex)
            {
                ReportError(ex.Message);
                _windowTracker.Dispose();
                _windowTracker = null;
            }
        }

        StartupStopwatch.Mark("window tracking started");

        // Dev mode uses the real taskbar replacement so the dock sits exactly where the taskbar is,
        // just like release. Only the data hot-reload service differs.
        StartReplacementMode();
        StartupStopwatch.Mark("taskbar replacement started");
        InitializeFeatures();
        StartupStopwatch.Mark("features initialized");
        startup.Finish("finalize total");
    }

    private (int Left, int Top, int Right, int Bottom)? GetDockPhysicalBounds()
    {
        if (_source is null || _source.Handle == 0 || !IsLoaded || DockFrame.ActualWidth <= 0 || DockFrame.ActualHeight <= 0)
            return null;
        try
        {
            var topLeft = DockFrame.PointToScreen(new Point(0, 0));
            var bottomRight = DockFrame.PointToScreen(new Point(DockFrame.ActualWidth, DockFrame.ActualHeight));
            // Subtract DockSurfaceTranslate so auto-hide overlap checks always test the dock's resting position.
            var matrix = _source.CompositionTarget?.TransformToDevice ?? Matrix.Identity;
            var offsetX = DockSurfaceTranslate.X * matrix.M11;
            var offsetY = DockSurfaceTranslate.Y * matrix.M22;
            return (
                (int)Math.Floor(topLeft.X - offsetX),
                (int)Math.Floor(topLeft.Y - offsetY),
                (int)Math.Ceiling(bottomRight.X - offsetX),
                (int)Math.Ceiling(bottomRight.Y - offsetY));
        }
        catch (InvalidOperationException)
        {
            return null;
        }
    }

    private void PositionDock(bool useWorkArea = false)
    {
        if (_replacementSession?.IsActive == true)
        {
            return;
        }

        var targetDisplay = DisplayService.GetTargetDisplay(_preferences.Current.TargetDisplay);
        // Before taskbar replacement takes over, WorkArea still excludes the native Windows taskbar (~48 DIP),
        // which would paint the dock slightly above the screen edge and then jump down when StartReplacementMode
        // runs. Align to monitor Bounds by default and reserve WorkArea for fallback when replacement fails.
        var area = useWorkArea ? targetDisplay.WorkArea : targetDisplay.Bounds;
        ResizeDockToContent();
        ApplyDockAlignment();
        if (IsVertical)
        {
            Left = Edge == DockEdge.Left ? area.Left : Math.Max(area.Left, area.Right - Width);
            Top = area.Top;
            Height = area.Height;
            return;
        }

        Left = _dockPosition switch
        {
            DockPosition.Left => area.Left,
            DockPosition.Center => area.Left + (area.Width - Width) / 2,
            DockPosition.Right => area.Right - Width,
            _ => area.Left
        };
        Top = Math.Max(area.Top, area.Bottom - Height);
    }

    private void StartReplacementMode()
    {
        var source = _source;
        if (source is null)
        {
            ReportError("ErrorDockNotReady");
            return;
        }

        ResizeDockToContent();
        ApplyDockAlignment();
        var targetDisplay = DisplayService.GetTargetDisplay(_preferences.Current.TargetDisplay);
        var session = new TaskbarReplacementSession(this, source.Handle, _preferences.Current.AutoHide != AutoHideMode.Disabled);
        session.OperationReported += ReportOperationStatus;
        session.LayoutChanged += ApplyTaskbarLayout;
        session.FullScreenChanged += OnFullScreenChanged;
        session.ShelfHeight = EffectiveShelfHeight;
        session.HeadroomHeight = EffectiveHeadroom;
        _appliedDockGeometry = (_preferences.Current.Size, _preferences.Current.DockScale, _preferences.Current.DockIconPadding, ExternalBottomGap, ExternalTopGap);
        session.Edge = Edge;
        session.TargetMonitor = targetDisplay.Handle;
        _replacementSession = session;
        try
        {
            session.Start();
            if (session.IsActive) TrayHost.Start();
        }
        catch (InvalidOperationException ex)
        {
            session.Dispose();
            if (!session.IsActive) _replacementSession = null;
            PositionDock(useWorkArea: true);
            ReportError(ex.Message);
        }
    }

    private IntPtr WindowProc(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (_taskbarCreatedMessage != 0 && unchecked((uint)message) == _taskbarCreatedMessage)
        {
            // Our own tray host broadcasts TaskbarCreated to collect icons; only an Explorer restart needs handling.
            if (!TrayHost.IsBroadcasting) _replacementSession?.HandleTaskbarCreated();
            handled = true;
            return IntPtr.Zero;
        }

        // The external top/bottom gaps are transparent parts of this window. Make them click-through so windows
        // behind the taskbar stay interactive and Windows does not treat the empty band as taskbar surface.
        if (message == WmNcHitTest && IsInExternalGap(lParam))
        {
            handled = true;
            return new IntPtr(HtTransparent);
        }

        // Keep the application's foreground state intact on left/middle clicks until the dock command runs.
        // Right-clicks open a ContextMenu, which needs window activation so WPF Popup mouse capture and focus stay open.
        if (message == WmMouseActivate)
        {
            var mouseMessage = (int)((lParam.ToInt64() >> 16) & 0xFFFF);
            const int wmRButtonDown = 0x0204;
            const int wmRButtonUp = 0x0205;
            if (mouseMessage != wmRButtonDown && mouseMessage != wmRButtonUp)
            {
                handled = true;
                return new IntPtr(MaNoActivate);
            }

            var currentFg = NativeWindowTracker.GetForegroundApplicationWindow();
            if (currentFg != 0) _foregroundWindow = currentFg;
        }

        // WM_DISPLAYCHANGE (displays added, removed, resized) or WM_SETTINGCHANGE/SPI_SETWORKAREA (taskbar strips
        // reserved or released) move the secondary docks.
        if (message == 0x007E || (message == 0x001A && wParam == 0x002F))
        {
            Dispatcher.BeginInvoke(DispatcherPriority.Background, ApplySecondaryDocks);
        }

        if (HandleHotkeyMessage(message, wParam))
        {
            handled = true;
            return IntPtr.Zero;
        }

        if (_replacementSession?.HandleMessage(unchecked((uint)message), wParam, lParam) == true)
        {
            handled = true;
        }

        return IntPtr.Zero;
    }

    /// <summary>
    /// True when a WM_NCHITTEST screen point falls in the transparent external gap around the dock surface rather
    /// than on the visible bar. Those regions must be click-through so maximized windows behind the gap stay
    /// interactive and the empty band is not treated as part of the taskbar by other applications.
    /// </summary>
    private bool IsInExternalGap(IntPtr lParam)
    {
        if (!IsLoaded || DockSurface is null || DockSurface.ActualWidth <= 0 || DockSurface.ActualHeight <= 0) return false;
        try
        {
            var coordinates = lParam.ToInt64();
            var screenX = (int)(short)(coordinates & 0xFFFF);
            var screenY = (int)(short)((coordinates >> 16) & 0xFFFF);
            var surfaceTopLeft = DockSurface.PointToScreen(new Point(0, 0));
            var surfaceBottomRight = DockSurface.PointToScreen(new Point(DockSurface.ActualWidth, DockSurface.ActualHeight));
            var withinSurface = screenX >= surfaceTopLeft.X && screenX <= surfaceBottomRight.X
                && screenY >= surfaceTopLeft.Y && screenY <= surfaceBottomRight.Y;
            if (withinSurface) return false;

            // Keep the screen-edge reveal strip hit-testable while auto-hide is enabled; otherwise the hidden
            // dock slides out of DockSurface and MouseEnter can never reach DockEdgeTrigger to reveal it again.
            if (_preferences.Current.AutoHide != AutoHideMode.Disabled
                && DockEdgeTrigger is { Visibility: Visibility.Visible, ActualWidth: > 0, ActualHeight: > 0 })
            {
                var triggerTopLeft = DockEdgeTrigger.PointToScreen(new Point(0, 0));
                var triggerBottomRight = DockEdgeTrigger.PointToScreen(new Point(DockEdgeTrigger.ActualWidth, DockEdgeTrigger.ActualHeight));
                if (screenX >= triggerTopLeft.X && screenX <= triggerBottomRight.X
                    && screenY >= triggerTopLeft.Y && screenY <= triggerBottomRight.Y)
                {
                    return false;
                }
            }

            return true;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
    }

    private void LoadPinnedApps(IReadOnlyList<string>? preferredOrder = null)
    {
        var result = WindowsTaskbarPinImporter.Import();
        var apps = result.Apps.ToList();
        foreach (var app in apps)
        {
            app.Icon = ApplyApplicationIcon(app);
        }

        if (preferredOrder is { Count: > 0 })
        {
            var orderMap = preferredOrder
                .Select((path, index) => (path, index))
                .ToDictionary(pair => pair.path, pair => pair.index, StringComparer.OrdinalIgnoreCase);

            apps.Sort((a, b) =>
            {
                var hasA = orderMap.TryGetValue(a.ExecutablePath, out var indexA);
                var hasB = orderMap.TryGetValue(b.ExecutablePath, out var indexB);
                if (hasA && hasB) return indexA.CompareTo(indexB);
                if (hasA) return -1;
                if (hasB) return 1;
                return 0;
            });
        }

        foreach (var app in apps)
        {
            _pinnedApps.Add(app);
        }

        if (result.ErrorKey is not null)
        {
            ReportError(result.ErrorKey);
        }
        else if (result.SkippedCount > 0) ReportError("SkippedPins", true, result.SkippedCount);
        RebuildDockOrder();
    }

    private void WindowTracker_WindowsChanged(object? sender, IReadOnlyList<TrackedWindow> windows)
    {
        windows = FilterWindows(windows);
        _runningAppIcons.Retain(windows);
        var foreground = NativeWindowTracker.GetForegroundApplicationWindow();
        if (foreground != 0)
        {
            _replacementSession?.UpdateForeground(foreground);
            _autoHideController?.CheckForegroundWindow(foreground);
        }
        // A stored HWND can outlive its window and be recycled, so validate the handle as well as the path.
        foreach (var (path, handle) in _lastSelectedWindows.ToList())
        {
            if (!windows.Any(window => window.Handle == handle
                    && string.Equals(window.ExecutablePath, path, StringComparison.OrdinalIgnoreCase)))
            {
                _lastSelectedWindows.Remove(path);
            }
        }
        var states = RunningWindowStates();
        UpdateNotificationBadges(_pinnedApps, states);
        UpdateWindowIndicators(states);
        RefreshRunningApps(RunningCandidates());
        UpdateSecondaryDocks();
        // Store app windows often publish their identity shortly after they appear, without another window event.
        if (_runningAppIcons.HasPendingLookups && !_iconRetryTimer.IsEnabled)
        {
            _iconRetryTimer.Start();
        }

        if (_thumbnailPreview.IsVisible)
        {
            var activeHandles = _thumbnailPreview.ActiveHandles;
            var closed = activeHandles.Where(h => windows.All(w => w.Handle != h)).ToList();
            if (closed.Count > 0 && _thumbnailPreview.Anchor is { Tag: string previewPath })
            {
                var remaining = windows
                    .Where(w => string.Equals(w.ExecutablePath, previewPath, StringComparison.OrdinalIgnoreCase))
                    .ToList();
                _thumbnailPreview.RemoveClosed(closed, remaining);
            }
        }
    }

    private bool _iconRefreshQueued;

    private void QueueIconRefresh()
    {
        if (_iconRefreshQueued) return;
        _iconRefreshQueued = true;
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            _iconRefreshQueued = false;
            if (_windowTracker is not null) WindowTracker_WindowsChanged(this, _windowTracker.Snapshot);
        });
    }

    private void IconRetryTimer_Tick(object? sender, EventArgs e)
    {
        _iconRetryTimer.Stop();
        if (_windowTracker is not null)
        {
            WindowTracker_WindowsChanged(this, _windowTracker.Snapshot);
        }
    }

    private void UpdateNotificationBadges(
        ObservableCollection<DockApp> apps,
        IReadOnlyList<RunningAppCandidate> candidates)
    {
        var notificationPaths = candidates.Where(candidate => candidate.HasNotification)
            .Select(candidate => candidate.ExecutablePath)
            .ToHashSet(StringComparer.OrdinalIgnoreCase);

        for (var index = 0; index < apps.Count; index++)
        {
            var app = apps[index];
            var hasNotification = notificationPaths.Contains(app.ExecutablePath);
            if (app.HasNotification != hasNotification)
            {
                app.HasNotification = hasNotification;
            }
        }
    }

    /// <summary>
    /// Every running window, including pinned apps. Badges and running indicators must see pinned apps too;
    /// only the running-app strip excludes them to avoid duplicate buttons.
    /// </summary>
    private IReadOnlyList<RunningAppCandidate> RunningWindowStates() =>
        RunningAppProjection.Project(VisibleSnapshot,
            new HashSet<string>(StringComparer.OrdinalIgnoreCase), _foregroundWindow,
            _preferences.Current.EffectiveNotificationBadgesEnabled, static _ => true);

    /// <summary>Current running apps after every gate: eligibility, virtual desktop, profile, pinned and disk path.</summary>
    private IReadOnlyList<RunningAppCandidate> RunningCandidates()
    {
        // Apps hidden by the active profile still show up among running apps while they have windows.
        var pinnedPaths = _pinnedApps.Where(app => !IsHiddenByProfile(app)).Select(app => app.ExecutablePath)
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        return RunningAppProjection.Project(VisibleSnapshot, pinnedPaths, _foregroundWindow,
            _preferences.Current.EffectiveNotificationBadgesEnabled, File.Exists);
    }

    /// <summary>Updates running apps and their layout together, including callers outside the window-event path.</summary>
    private void RefreshRunningApps(IReadOnlyList<RunningAppCandidate>? candidates = null)
    {
        if (UpdateRunningApps(candidates ?? RunningCandidates()))
        {
            RebuildDockOrder();
            ResizeDockToContent();
            _replacementSession?.RefreshPosition();
        }
    }

    private bool UpdateRunningApps(IReadOnlyList<RunningAppCandidate> candidates)
    {
        var desiredPaths = candidates.Select(candidate => candidate.ExecutablePath)
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        foreach (var executablePath in desiredPaths)
        {
            CancelRunningAppExit(executablePath);
        }

        var currentApps = _runningApps.ToDictionary(app => app.ExecutablePath, StringComparer.OrdinalIgnoreCase);
        var desiredApps = candidates
            .Select(candidate =>
            {
                var executablePath = candidate.ExecutablePath;
                var icon = _runningAppIcons.Get(executablePath, candidate.IconWindow);
                if (currentApps.TryGetValue(executablePath, out var existingApp))
                {
                    // While this app's icon loads in the background, keep showing the current one. Other apps must
                    // still be free to update theirs.
                    if (icon is not null || !_runningAppIcons.IsLoadingFor(executablePath, candidate.IconWindow)) existingApp.Icon = icon;
                    existingApp.HasNotification = candidate.HasNotification;
                    existingApp.IsForeground = candidate.IsForeground;
                    existingApp.WindowCount = candidate.WindowCount;
                    return existingApp;
                }

                return new DockApp(GetApplicationName(executablePath), executablePath)
                {
                    Icon = icon,
                    HasNotification = candidate.HasNotification,
                    IsForeground = candidate.IsForeground,
                    WindowCount = candidate.WindowCount
                };
            })
            .OrderBy(app =>
            {
                var existingIndex = -1;
                for (var i = 0; i < _runningApps.Count; i++)
                {
                    if (string.Equals(_runningApps[i].ExecutablePath, app.ExecutablePath, StringComparison.OrdinalIgnoreCase))
                    {
                        existingIndex = i;
                        break;
                    }
                }
                return existingIndex >= 0 ? existingIndex : int.MaxValue;
            })
            .ThenBy(app => app.Name, StringComparer.CurrentCultureIgnoreCase)
            .ToList();
        var layoutChanged = !_runningApps.Select(app => app.ExecutablePath)
            .SequenceEqual(desiredApps.Select(app => app.ExecutablePath), StringComparer.OrdinalIgnoreCase);

        for (var desiredIndex = 0; desiredIndex < desiredApps.Count; desiredIndex++)
        {
            var desiredApp = desiredApps[desiredIndex];
            if (desiredIndex < _runningApps.Count
                && string.Equals(_runningApps[desiredIndex].ExecutablePath, desiredApp.ExecutablePath, StringComparison.OrdinalIgnoreCase))
            {
                if (_runningApps[desiredIndex] != desiredApp)
                {
                    _runningApps[desiredIndex] = desiredApp;
                }

                continue;
            }

            var existingIndex = -1;
            for (var index = desiredIndex + 1; index < _runningApps.Count; index++)
            {
                if (string.Equals(_runningApps[index].ExecutablePath, desiredApp.ExecutablePath, StringComparison.OrdinalIgnoreCase))
                {
                    existingIndex = index;
                    break;
                }
            }

            if (existingIndex >= 0)
            {
                _runningApps.Move(existingIndex, desiredIndex);
                if (_runningApps[desiredIndex] != desiredApp)
                {
                    _runningApps[desiredIndex] = desiredApp;
                }
            }
            else
            {
                _runningApps.Insert(desiredIndex, desiredApp);
            }
        }

        for (var index = _runningApps.Count - 1; index >= desiredApps.Count; index--)
        {
            AnimateRunningAppRemoval(_runningApps[index]);
        }

        RunningAppsDivider.Visibility = Visibility.Collapsed;
        UpdateExtrasDividerVisibility();
        return layoutChanged;
    }

    private void CancelRunningAppExit(string executablePath)
    {
        if (!_exitingRunningPaths.Remove(executablePath))
        {
            return;
        }

        var app = _runningApps.FirstOrDefault(candidate =>
            string.Equals(candidate.ExecutablePath, executablePath, StringComparison.OrdinalIgnoreCase));
        var button = app is null ? null : FindVisualChild<Button>(RunningApps.ItemContainerGenerator.ContainerFromItem(app));
        if (button is null) return;

        button.BeginAnimation(UIElement.OpacityProperty, null);
        button.Opacity = 1;
        button.IsHitTestVisible = true;
        if (button.RenderTransform is ScaleTransform { IsFrozen: false, IsSealed: false } scale)
        {
            scale.BeginAnimation(ScaleTransform.ScaleXProperty, null);
            scale.BeginAnimation(ScaleTransform.ScaleYProperty, null);
            scale.ScaleX = 1;
            scale.ScaleY = 1;
        }
    }

    private void AnimateRunningAppRemoval(DockApp app)
    {
        if (!_exitingRunningPaths.Add(app.ExecutablePath))
        {
            return;
        }

        AnimateDockAppRemoval(RunningApps, _runningApps, app, () =>
        {
            _exitingRunningPaths.Remove(app.ExecutablePath);
            ResizeDockToContent();
            _replacementSession?.RefreshPosition();
        });
    }

    private void AnimateDockAppRemoval(
        ItemsControl owner,
        ObservableCollection<DockApp> apps,
        DockApp app,
        Action afterRemoval)
    {
        var button = FindVisualChild<Button>(owner.ItemContainerGenerator.ContainerFromItem(app));
        if (!_preferences.Current.EffectiveAnimationsEnabled || !SystemParameters.ClientAreaAnimation || button?.RenderTransform is not ScaleTransform { IsFrozen: false, IsSealed: false } scale)
        {
            RemoveDockApp(apps, app);
            afterRemoval();
            return;
        }

        button.IsHitTestVisible = false;
        var fade = new DoubleAnimation(0, TimeSpan.FromMilliseconds(75))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
        };
        fade.Completed += (_, _) =>
        {
            if (ReferenceEquals(owner, RunningApps) && !_exitingRunningPaths.Contains(app.ExecutablePath))
            {
                return;
            }
            RemoveDockApp(apps, app);
            afterRemoval();
        };
        button.BeginAnimation(UIElement.OpacityProperty, fade);
        scale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(0.95, TimeSpan.FromMilliseconds(75))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
        });
        scale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(0.95, TimeSpan.FromMilliseconds(75))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
        });
    }

    private static void RemoveDockApp(ObservableCollection<DockApp> apps, DockApp app)
    {
        var index = apps.IndexOf(app);
        if (index >= 0)
        {
            apps.RemoveAt(index);
        }
    }

    private static T? FindVisualChild<T>(DependencyObject? parent) where T : DependencyObject
    {
        if (parent is null)
        {
            return null;
        }

        for (var index = 0; index < VisualTreeHelper.GetChildrenCount(parent); index++)
        {
            var child = VisualTreeHelper.GetChild(parent, index);
            if (child is T match)
            {
                return match;
            }

            var descendant = FindVisualChild<T>(child);
            if (descendant is not null)
            {
                return descendant;
            }
        }

        return null;
    }

    private void DockAppButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (!_preferences.Current.EffectiveAnimationsEnabled || !SystemParameters.ClientAreaAnimation || sender is not Button button)
        {
            return;
        }

        var scale = new ScaleTransform();
        button.RenderTransform = scale;
        // Keep the new application visible on its first rendered frame.
        button.Opacity = 1;
        scale.ScaleX = 0.95;
        scale.ScaleY = 0.95;
        var duration = TimeSpan.FromMilliseconds(165);
        var easing = new CubicEase { EasingMode = EasingMode.EaseOut };
        scale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(0.95, 1, duration) { EasingFunction = easing });
        scale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(0.95, 1, duration) { EasingFunction = easing });
    }

    private void DockApp_PreviewKeyDown(object sender, System.Windows.Input.KeyEventArgs e)
    {
        _foregroundAtAppPress = null;
    }

    private void DockApp_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { DataContext: DockApp app })
        {
            return;
        }

        var foregroundAtPress = _foregroundAtAppPress;
        _foregroundAtAppPress = null;
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        AnimateActivationFeedback((Button)sender);
        var modifiers = System.Windows.Input.Keyboard.Modifiers;
        if (modifiers.HasFlag(System.Windows.Input.ModifierKeys.Shift))
        {
            LaunchApp(app, elevated: modifiers.HasFlag(System.Windows.Input.ModifierKeys.Control));
            return;
        }

        OpenDockApp(app, toggleIfForeground: true, foregroundAtPress);
    }

    private void DockApp_MouseUp(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        if (e.ChangedButton != System.Windows.Input.MouseButton.Middle || sender is not Button { DataContext: DockApp app }) return;
        e.Handled = true;
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        AnimateActivationFeedback((Button)sender);
        LaunchApp(app, elevated: false);
    }

    /// <summary>Wheel over an app cycles through its windows; elsewhere on the dock it changes the volume.</summary>
    private void DockApp_MouseWheel(object sender, System.Windows.Input.MouseWheelEventArgs e)
    {
        if (sender is not Button { DataContext: DockApp app }) return;
        e.Handled = true;
        var windows = VisibleSnapshot
            .Where(window => string.Equals(window.ExecutablePath, app.ExecutablePath, StringComparison.OrdinalIgnoreCase))
            .OrderBy(window => window.Handle.ToInt64())
            .ToList();
        if (windows is not { Count: > 0 }) return;
        var current = windows.FindIndex(window => window.Handle == _foregroundWindow);
        if (current < 0 && _lastSelectedWindows.TryGetValue(app.ExecutablePath, out var last))
            current = windows.FindIndex(window => window.Handle == last);
        var step = e.Delta < 0 ? 1 : -1;
        var next = windows[((current < 0 ? (step > 0 ? -1 : 0) : current) + step + windows.Count) % windows.Count];
        _thumbnailPreview.Hide();
        if (NativeWindowTracker.Activate(next.Handle)) _lastSelectedWindows[app.ExecutablePath] = next.Handle;
    }

    private void DockSurface_MouseWheel(object sender, System.Windows.Input.MouseWheelEventArgs e)
    {
        if (e.Handled || !_preferences.Current.WheelVolume) return;
        e.Handled = true;
        try
        {
            _wheelAudio ??= AudioEndpointMonitor.AcquireShared();
        }
        catch (COMException ex)
        {
            AppLog.Warning($"Core Audio unavailable for wheel volume: {ex.Message}");
            return;
        }

        if (!_wheelAudio.HasDevice) return;
        // One wheel notch (120) = 2%. Touchpads send many small deltas; accumulate them instead of 2% per event.
        _wheelRemainder += e.Delta;
        var notches = _wheelRemainder / 120;
        if (notches == 0) return;
        _wheelRemainder -= notches * 120;
        var level = Math.Clamp(_wheelAudio.Volume + notches * 0.02f, 0f, 1f);
        if (!_wheelAudio.SetVolume(level)) ReportError("ErrorVolumeChange");
    }

    private void StartButton_Click(object sender, RoutedEventArgs e)
    {
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();

        var startMenuWasOpen = _startMenuOpenAtPress || _startMenu.IsOpen || _startMenu.IsStartMenuOpenNow();
        _startMenuOpenAtPress = false;
        if (startMenuWasOpen)
        {
            CloseStartMenu();
            return;
        }

        const uint wmSysCommand = 0x0112;
        const int scTaskList = 0xF130;
        if (_source is null || !PostMessage(_source.Handle, wmSysCommand, scTaskList, 0))
        {
            ReportError("ErrorStartMenuOpen");
        }
        else
        {
            ShellShortcuts.ScheduleStartMenuReposition(StartButton);
        }
    }

    /// <summary>
    /// Closes the open Start menu with Escape and the Windows key, then hands the foreground back to the application that
    /// was in front before it, as the native taskbar does. Windows otherwise returns it to the dock window.
    /// </summary>
    private void CloseStartMenu()
    {
        _startMenu.Reset();
        var startWindows = ShellShortcuts.FindStartMenuWindows();
        if (startWindows.Count > 0)
        {
            var fg = GetForegroundWindow();
            if (fg != startWindows[0] && !_startMenu.IsStartMenuWindow(fg))
            {
                SetForegroundWindow(startWindows[0]);
            }
        }
        SyntheticKeyboard.SendChord(0x1B);

        var application = _lastApplicationWindow;
        if (application == 0) return;
        var restoreTimer = new DispatcherTimer(DispatcherPriority.Normal) { Interval = TimeSpan.FromMilliseconds(200) };
        restoreTimer.Tick += (_, _) =>
        {
            restoreTimer.Stop();
            if (_startMenu.IsStartMenuOpenNow() || !IsWindow(application) || IsIconic(application)) return;
            // Only take the foreground back from the dock or the closed Start menu, never from a window the
            // user focused in the meantime.
            var current = GetForegroundWindow();
            GetWindowThreadProcessId(current, out var pid);
            if (pid == Environment.ProcessId || _startMenu.IsStartMenuWindow(current))
            {
                SetForegroundWindow(application);
            }
        };
        restoreTimer.Start();
    }

    private void OpenDockApp(DockApp app, bool toggleIfForeground = false, nint? foregroundAtPress = null)
    {
        var executablePath = app.ExecutablePath;
        var existingWindow = foregroundAtPress is { } pressedWindow
            ? _windowTracker?.Snapshot.FirstOrDefault(window => window.Handle == pressedWindow
                && string.Equals(window.ExecutablePath, executablePath, StringComparison.OrdinalIgnoreCase))
            : null;
        existingWindow ??= FindNextWindowForExecutable(executablePath);
        if (existingWindow is not null)
        {
            var activated = toggleIfForeground
                ? NativeWindowTracker.ToggleActivation(existingWindow.Handle, executablePath, foregroundAtPress)
                : NativeWindowTracker.Activate(existingWindow.Handle);
            if (!activated)
            {
                ReportError("ErrorWindowActivate");
                return;
            }

            _lastSelectedWindows[executablePath] = existingWindow.Handle;
            return;
        }

        LaunchApp(app, elevated: false);
    }

    /// <summary>Starts a new instance even when the app already has windows (Shift/middle click, Ctrl+Shift for admin).</summary>
    private void LaunchApp(DockApp app, bool elevated)
    {
        var executablePath = app.ExecutablePath;
        var arguments = app.LaunchArguments ?? string.Empty;
        // ShellExecuteEx loads shell extensions and can take from 100 ms to over a second. On the UI thread it froze
        // the dock, launch bounce included, until Windows had taken the app. Start it from the thread pool instead;
        // Process.Start runs ShellExecuteEx on its own STA thread when called from an MTA thread.
        _ = Task.Run(() => StartApplication(executablePath, arguments, elevated)).ContinueWith(task =>
        {
            if (task.IsCompletedSuccessfully)
            {
                if (task.Result is { } error) Dispatcher.BeginInvoke(() => ReportError(error));
                return;
            }

            AppLog.Warning($"Failed to start application: {task.Exception?.GetBaseException().Message}");
            Dispatcher.BeginInvoke(() => ReportError("ErrorAppLaunchFailed"));
        }, TaskScheduler.Default);
    }

    /// <summary>Starts an application through the shell and returns an error key, or null on success or UAC cancel.</summary>
    internal static string? StartApplication(string executablePath, string arguments, bool elevated)
    {
        if (!File.Exists(executablePath)) return "ErrorAppFileNotFound";
        try
        {
            using var _ = Process.Start(new ProcessStartInfo(executablePath)
            {
                Arguments = arguments,
                UseShellExecute = true,
                Verb = elevated ? "runas" : string.Empty
            });
            return null;
        }
        catch (Win32Exception ex) when (elevated && ex.NativeErrorCode == 1223)
        {
            // The user declined the UAC prompt.
            return null;
        }
        catch (Win32Exception)
        {
            return "ErrorAppLaunchWin32";
        }
        catch (InvalidOperationException)
        {
            return "ErrorAppLaunchFailed";
        }
        catch (UnauthorizedAccessException)
        {
            return "ErrorAppLaunchDenied";
        }
        catch (IOException)
        {
            return "ErrorAppFileInaccessible";
        }
    }

    private void DockApp_MouseEnter(object sender, System.Windows.Input.MouseEventArgs e)
    {
        _previewCloseTimer.Stop();
        _previewHoverTimer.Stop();
        _pendingPreviewButton = null;
        if (_widgetPanel.IsOpen) return;
        if (!_preferences.Current.PreviewsEnabled)
        {
            if (sender is Button button) ToolTipService.SetIsEnabled(button, true);
            _thumbnailPreview.Hide();
            return;
        }
        if (sender is not Button { Tag: string executablePath } hoveredButton)
        {
            return;
        }

        var matchingWindows = VisibleSnapshot
            .Where(window => string.Equals(window.ExecutablePath, executablePath, StringComparison.OrdinalIgnoreCase))
            .ToList() ?? [];

        ToolTipService.SetIsEnabled(hoveredButton, matchingWindows.Count == 0);
        if (matchingWindows.Count == 0)
        {
            _thumbnailPreview.Hide();
            return;
        }

        _pendingPreviewButton = hoveredButton;
        _previewHoverTimer.Interval = TimeSpan.FromMilliseconds(_thumbnailPreview.IsVisible ? 140 : 300);
        _previewHoverTimer.Start();
    }

    private void PreviewHoverTimer_Tick(object? sender, EventArgs e)
    {
        _previewHoverTimer.Stop();
        var hoveredButton = _pendingPreviewButton;
        _pendingPreviewButton = null;
        if (hoveredButton is null || !hoveredButton.IsMouseOver || _widgetPanel.IsOpen || !_preferences.Current.PreviewsEnabled)
        {
            return;
        }
        if (hoveredButton.Tag is not string executablePath)
        {
            return;
        }

        var matchingWindows = VisibleSnapshot
            .Where(window => string.Equals(window.ExecutablePath, executablePath, StringComparison.OrdinalIgnoreCase))
            .ToList() ?? [];
        if (matchingWindows.Count == 0)
        {
            _thumbnailPreview.Hide();
            return;
        }

        var appIcon = hoveredButton.DataContext is DockApp app ? app.Icon : null;
        _thumbnailPreview.ExtraGapDips = _preferences.Current.MagnificationEnabled
            ? DockFisheyeLayout.ComputePreviewClearance(Profile.Icon, CurrentMagnificationMaxScale)
            : 0;
        _thumbnailPreview.ShowForWindows(matchingWindows, hoveredButton, appIcon);
    }

    private void DockApp_MouseLeave(object sender, System.Windows.Input.MouseEventArgs e)
    {
        _previewHoverTimer.Stop();
        _pendingPreviewButton = null;
        _previewCloseTimer.Stop();
        _previewCloseTimer.Start();
    }

    private void PreviewCloseTimer_Tick(object? sender, EventArgs e)
    {
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        _autoHideController?.EvaluateState();
    }

    private void ThumbnailPreview_PointerEntered(object? sender, EventArgs e)
    {
        _previewCloseTimer.Stop();
    }

    private void ThumbnailPreview_PointerLeft(object? sender, EventArgs e)
    {
        _previewCloseTimer.Stop();
        _previewCloseTimer.Start();
    }

    private void ThumbnailPreview_PreviewClicked(object? sender, EventArgs e)
    {
        var sourceHandle = _thumbnailPreview.SourceHandle;
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        _autoHideController?.EvaluateState();
        if (sourceHandle == 0 || !NativeWindowTracker.Activate(sourceHandle))
        {
            ReportError("ErrorPreviewActivate");
            return;
        }

    }

    private void ThumbnailPreview_CloseRequested(object? sender, EventArgs e)
    {
        // The preview stays open: the window-list update removes the closed card once the window is gone.
        var sourceHandle = _thumbnailPreview.SourceHandle;
        _previewCloseTimer.Stop();
        if (sourceHandle == 0 || !NativeWindowTracker.RequestClose(sourceHandle))
        {
            ReportError("ErrorPreviewClose");
            return;
        }

    }

    private TrackedWindow? FindWindowForExecutable(string executablePath)
    {
        return VisibleSnapshot.FirstOrDefault(window => window.Handle == _foregroundWindow
            && string.Equals(window.ExecutablePath, executablePath, StringComparison.OrdinalIgnoreCase))
            ?? VisibleSnapshot.FirstOrDefault(window =>
                string.Equals(window.ExecutablePath, executablePath, StringComparison.OrdinalIgnoreCase));
    }

    private TrackedWindow? FindNextWindowForExecutable(string executablePath)
    {
        var matchingWindows = VisibleSnapshot.Where(window =>
            string.Equals(window.ExecutablePath, executablePath, StringComparison.OrdinalIgnoreCase)).ToList();
        if (matchingWindows is not { Count: > 0 })
        {
            return null;
        }

        var foreground = matchingWindows.FirstOrDefault(window => window.Handle == _foregroundWindow);
        if (foreground is not null)
        {
            return foreground;
        }

        matchingWindows.Sort((left, right) => left.Handle.ToInt64().CompareTo(right.Handle.ToInt64()));
        var previousIndex = _lastSelectedWindows.TryGetValue(executablePath, out var previousHandle)
            ? matchingWindows.FindIndex(window => window.Handle == previousHandle)
            : -1;
        return matchingWindows[(previousIndex + 1) % matchingWindows.Count];
    }

    private void UpdateWindowIndicators(IReadOnlyList<RunningAppCandidate> candidates)
    {
        var states = candidates.ToDictionary(
            candidate => candidate.ExecutablePath,
            candidate => (candidate.WindowCount, candidate.IsForeground),
            StringComparer.OrdinalIgnoreCase);

        for (var index = 0; index < _pinnedApps.Count; index++)
        {
            var app = _pinnedApps[index];
            states.TryGetValue(app.ExecutablePath, out var state);
            var isForeground = state.IsForeground;
            var count = state.WindowCount;
            if (app.IsRunning != (count > 0) || app.IsForeground != isForeground || app.WindowCount != count)
            {
                app.IsForeground = isForeground;
                app.WindowCount = count;
            }
        }
    }

    private static string GetApplicationName(string executablePath)
    {
        var description = FileVersionInfo.GetVersionInfo(executablePath).FileDescription;
        return string.IsNullOrWhiteSpace(description)
            ? Path.GetFileNameWithoutExtension(executablePath)
            : description.Trim();
    }

    private bool IsShellOverlayWindow(nint windowHandle) =>
        _startMenu.IsStartMenuWindow(windowHandle) || ShellOverlayWindows.IsTaskSwitcher(windowHandle);

    /// <summary>
    /// Lightweight fallback for focus transitions that do not raise EVENT_SYSTEM_FOREGROUND
    /// (minimizing, alt-tab, switching desktops). Keeps indicator dots in sync.
    /// </summary>
    private void ForegroundPollTimer_Tick(object? sender, EventArgs e)
    {
        if (_windowTracker is null) return;
        var current = NativeWindowTracker.GetForegroundApplicationWindow();
        if (current != _foregroundWindow)
            WindowTracker_ForegroundWindowChanged(current);
    }

    private void WindowTracker_ForegroundWindowChanged(nint windowHandle)
    {
        var previousForeground = _foregroundWindow;
        // Publish the new handle before secondary docks and indicators read it; otherwise they keep the
        // previous app active until the next snapshot change or the 10-second fallback timer.
        if (previousForeground != windowHandle)
        {
            _foregroundWindow = windowHandle;
            var candidates = RunningWindowStates();
            UpdateWindowIndicators(candidates);
            foreach (var app in _runningApps)
            {
                var isForeground = candidates.Any(candidate => candidate.IsForeground
                    && string.Equals(candidate.ExecutablePath, app.ExecutablePath, StringComparison.OrdinalIgnoreCase));
                if (app.IsForeground != isForeground) app.IsForeground = isForeground;
            }
        }

        UpdateSecondaryDocks();
        _replacementSession?.UpdateForeground(windowHandle);
        _autoHideController?.CheckForegroundWindow(windowHandle);
        GetWindowThreadProcessId(windowHandle, out var processId);
        // Dock buttons, thumbnail hosts, and menus must not replace the last external foreground window.
        if (processId == Environment.ProcessId) return;
        var isStartMenu = _startMenu.IsStartMenuWindow(windowHandle);
        _startMenu.OnForegroundChanged(isStartMenu);
        if (!isStartMenu && windowHandle != 0 && !ShellOverlayWindows.IsTaskSwitcher(windowHandle))
            _lastApplicationWindow = windowHandle;
        if (_openContextMenu is not null
            && windowHandle != 0
            && windowHandle != previousForeground
            && windowHandle != _foregroundAtMenuOpen
            && (DateTime.UtcNow - _contextMenuOpenedAtUtc).TotalMilliseconds > 250)
        {
            _openContextMenu.IsOpen = false;
        }
    }

    private static void ReportError(string localizationKey) => AppLog.ReportError(localizationKey);

    private static void ReportOperationStatus(string localizationKey, bool isError) => ReportError(localizationKey, isError);

    /// <summary>Failures and operational notes go to the log only; the dock stays silent.</summary>
    private static void ReportError(string localizationKey, bool isError, params object[] arguments)
    {
        if (isError) AppLog.ReportError(localizationKey, arguments);
        else AppLog.Info(AppLog.ResolveMessage(localizationKey, arguments));
    }

    private async void DockContextMenu_Opened(object sender, RoutedEventArgs e)
    {
        _widgetPanel.Close(restoreFocus: false);
        if (sender is not ContextMenu contextMenu)
        {
            return;
        }

        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        ResetMagnification();
        if (_openContextMenu is not null && !ReferenceEquals(_openContextMenu, contextMenu))
            _openContextMenu.IsOpen = false;
        _openContextMenu = contextMenu;
        _foregroundAtMenuOpen = _foregroundWindow;
        _contextMenuOpenedAtUtc = DateTime.UtcNow;
        if (_source is not null && _source.Handle != 0)
        {
            SetForegroundWindow(_source.Handle);
        }

        _contextTargetElement = contextMenu.PlacementTarget as FrameworkElement
            ?? FindVisualParent<FrameworkElement>(contextMenu.PlacementTarget);
        _contextTargetWidget = _contextTargetElement as OpenTaskbar.Widgets.DockWidget
            ?? FindVisualParent<OpenTaskbar.Widgets.DockWidget>(contextMenu.PlacementTarget);
        var pinnedApp = (contextMenu.PlacementTarget as FrameworkElement)?.DataContext as DockApp;
        contextMenu.DataContext = (contextMenu.PlacementTarget as FrameworkElement)?.DataContext;
        _contextTargetWasForeground = pinnedApp?.IsForeground == true;
        var hasAppTarget = pinnedApp is not null;
        var isPinnedTarget = pinnedApp?.ShortcutPath is not null;
        var isRunning = pinnedApp?.IsRunning == true;
        SetMenuItemVisibility(contextMenu, "ActivateDockApp", hasAppTarget);
        SetMenuItemVisibility(contextMenu, "AddToGroup", hasAppTarget);
        SetMenuItemVisibility(contextMenu, "NewGroup", !hasAppTarget);
        SetMenuItemVisibility(contextMenu, "CloseDockApp", isRunning);
        SetMenuItemVisibility(contextMenu, "TerminateDockApp", isRunning);
        SetMenuItemVisibility(contextMenu, "PinDockApp", hasAppTarget && !isPinnedTarget);
        SetMenuItemVisibility(contextMenu, "UnpinDockApp", isPinnedTarget);
        SetSeparatorVisibility(contextMenu, "RunningCommandsDivider", isRunning);
        SetMenuItemVisibility(contextMenu, "HideInProfile", isPinnedTarget && ActiveProfile is not null);
        if (FindMenuItem(contextMenu, "AddToGroup") is { } addToGroupMenu) PopulateAddToGroupMenu(addToGroupMenu, pinnedApp);
        if (FindMenuItem(contextMenu, "ProfilesMenu") is { } profilesMenu) PopulateProfilesMenu(profilesMenu);
        if (FindMenuItem(contextMenu, "FocusModeMenu") is { } focusModeMenu) focusModeMenu.IsChecked = _preferences.Current.FocusModeEnabled;
        if (FindMenuItem(contextMenu, "UpdateMenu") is { } updateMenu)
        {
            updateMenu.Header = _availableUpdate is { } update
                ? UiText.Format("UpdateDownload", update.Tag)
                : UiText.Get("CheckForUpdates");
        }

        if (pinnedApp is null)
        {
            var isWidget = _contextTargetWidget is not null;
            SetMenuItemVisibility(contextMenu, "RemoveWidgetMenu", isWidget);
            FocusContextMenu(contextMenu);
            return;
        }

        SetMenuCommandParameter(contextMenu, pinnedApp.ExecutablePath);
        var targetWindow = FindWindowForExecutable(pinnedApp.ExecutablePath);
        if (FindMenuItem(contextMenu, "ActivateDockApp") is { } activateMenuItem)
        {
            activateMenuItem.CommandParameter = pinnedApp;
            activateMenuItem.Header = isRunning
                ? (pinnedApp.IsForeground ? UiText.Get("MinimizeWindow") : UiText.Get("ActivateWindow"))
                : UiText.Get("OpenApplication");
        }
        if (FindMenuItem(contextMenu, "PinDockApp") is { } pinMenuItem)
        {
            pinMenuItem.CommandParameter = pinnedApp;
        }
        if (FindMenuItem(contextMenu, "UnpinDockApp") is { } unpinMenuItem)
        {
            unpinMenuItem.CommandParameter = pinnedApp;
        }
        foreach (var tag in new[] { "HideInProfile", "NewInstance", "RunAsAdmin" })
        {
            if (FindMenuItem(contextMenu, tag) is { } item) item.CommandParameter = pinnedApp;
        }
        FocusContextMenu(contextMenu);
        await LoadJumpListAsync(contextMenu, targetWindow?.Handle ?? 0, pinnedApp.ShortcutPath);
    }

    private async Task LoadJumpListAsync(ContextMenu menu, nint window, string? shortcut)
    {
        var boundary = menu.Items.OfType<Separator>().FirstOrDefault(item => item.Tag as string == "JumpListCommands");
        if (boundary is null) return;
        _jumpListLoad?.Cancel();
        _jumpListLoad?.Dispose();
        var load = new CancellationTokenSource(TimeSpan.FromSeconds(3));
        _jumpListLoad = load;
        ReplaceJumpListItems(menu, boundary, [new MenuItem { Header = UiText.Get("TasksLoading"), IsEnabled = false, Focusable = false }]);
        try
        {
            var result = await _jumpLists.ReadAsync(window, shortcut, load.Token);
            if (!menu.IsOpen || !ReferenceEquals(_jumpListLoad, load))
            {
                await _jumpLists.ReleaseAsync(result.SessionId);
                return;
            }
            _jumpListSession = result.SessionId;
            var items = new List<Control>();
            foreach (var group in result.Groups)
            {
                if (items.Count > 0) items.Add(new Separator());
                var heading = new MenuItem { Header = group.Title.Replace("_", "__") };
                heading.SetResourceReference(StyleProperty, "MenuSectionHeading");
                items.Add(heading);
                foreach (var entry in group.Entries)
                {
                    if (entry.IsSeparator) { items.Add(new Separator()); continue; }
                    // Keep the session alive until Click queues invocation; WPF normally closes first.
                    var item = new MenuItem { Header = entry.Title.Replace("_", "__"), StaysOpenOnClick = true };
                    if (entry.Icon is not null) item.Icon = new Image { Source = entry.Icon, Width = 16, Height = 16 };
                    item.Click += async (_, args) =>
                    {
                        args.Handled = true;
                        // Await before closing: closing raises ContextMenu.Closed -> ReleaseAsync, and racing both
                        // on the COM worker can dispose the session before the invocation is dispatched.
                        var launched = await _jumpLists.InvokeAsync(result.SessionId, entry.CommandId, _source?.Handle ?? 0);
                        menu.IsOpen = false;
                        if (!launched && IsLoaded)
                            ReportError("ErrorAppTaskRun");
                    };
                    items.Add(item);
                }
            }
            if (result.Status != JumpListStatus.Ready)
            {
                var message = result.Status switch
                {
                    JumpListStatus.Empty => UiText.Get("TasksEmpty"),
                    JumpListStatus.IdentityUnavailable => UiText.Get("TasksIdentityUnavailable"),
                    JumpListStatus.Unsupported => UiText.Get("TasksUnsupported"),
                    _ => UiText.Get("TasksReadError")
                };
                items.Add(new MenuItem { Header = message, IsEnabled = false });
            }
            ReplaceJumpListItems(menu, boundary, items);
        }
        catch (OperationCanceledException)
        {
            if (menu.IsOpen && ReferenceEquals(_jumpListLoad, load))
            {
                ReplaceJumpListItems(menu, boundary, [new MenuItem { Header = UiText.Get("TasksTimeout"), IsEnabled = false }]);
            }
        }
        catch (COMException ex)
        {
            Trace.TraceWarning("Shell worker failed: 0x{0:X8}", ex.HResult);
            if (menu.IsOpen && ReferenceEquals(_jumpListLoad, load))
            {
                ReplaceJumpListItems(menu, boundary, [new MenuItem { Header = UiText.Get("TasksServiceError"), IsEnabled = false }]);
            }
        }
    }

    private static void ReplaceJumpListItems(ContextMenu menu, Separator boundary, IReadOnlyList<Control> items)
    {
        foreach (var previous in menu.Items.OfType<Control>().Where(item => item.Tag as string == "JumpListEntry").ToArray())
            menu.Items.Remove(previous);
        var index = menu.Items.IndexOf(boundary);
        foreach (var item in items)
        {
            item.Tag = "JumpListEntry";
            if (!item.IsEnabled) item.Focusable = false;
            menu.Items.Insert(index++, item);
        }
        boundary.Visibility = items.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        if (menu.IsOpen && !menu.IsKeyboardFocusWithin)
        {
            menu.Focus();
        }
    }

    private static void FocusContextMenu(ContextMenu menu)
    {
        menu.Focus();
    }

    private async void DockContextMenu_Closed(object sender, RoutedEventArgs e)
    {
        if (sender is ContextMenu closedMenu)
        {
            closedMenu.DataContext = null;
            SetMenuCommandParameter(closedMenu, null);
            if (closedMenu.Items.OfType<Separator>().FirstOrDefault(item => item.Tag as string == "JumpListCommands") is { } boundary)
            {
                ReplaceJumpListItems(closedMenu, boundary, []);
            }
        }
        if (!ReferenceEquals(sender, _openContextMenu)) return;
        _openContextMenu = null;
        _contextTargetWidget = null;
        _contextTargetElement = null;
        _autoHideController?.EvaluateState();
        _jumpListLoad?.Cancel();
        if (_jumpListSession is { } session)
        {
            _jumpListSession = null;
            await _jumpLists.ReleaseAsync(session);
        }
    }

    private void RemoveWidgetMenu_Click(object sender, RoutedEventArgs e)
    {
        var target = (sender as MenuItem)?.Parent is ContextMenu menu ? menu.PlacementTarget : null;
        var element = target as FrameworkElement ?? _contextTargetElement;
        var widget = element as OpenTaskbar.Widgets.DockWidget ?? FindVisualParent<OpenTaskbar.Widgets.DockWidget>(element) ?? _contextTargetWidget;
        if (widget?.Tag is string widgetId)
        {
            RemoveDockWidget(widgetId);
            return;
        }
        if (widget is not null && _widgetCoordinator.GetWidgetId(widget) is { } id)
        {
            RemoveDockWidget(id);
            return;
        }
        if (element is { Tag: string elementTag } && !string.IsNullOrWhiteSpace(elementTag))
        {
            RemoveDockWidget(elementTag);
            return;
        }
        if (element is { DataContext: DockWidgetItem dwi })
        {
            RemoveDockWidget(dwi.Id);
            return;
        }
        var parent = FindVisualParentWithTag(element);
        if (parent?.Tag is string parentTag && !string.IsNullOrWhiteSpace(parentTag))
        {
            RemoveDockWidget(parentTag);
            return;
        }
    }

    private static FrameworkElement? FindVisualParentWithTag(DependencyObject? child)
    {
        while (child is not null)
        {
            if (child is FrameworkElement { Tag: string tag } fe && !string.IsNullOrWhiteSpace(tag)) return fe;
            child = VisualTreeHelper.GetParent(child);
        }
        return null;
    }

    private static T? FindVisualParent<T>(DependencyObject? child) where T : DependencyObject
    {
        while (child is not null)
        {
            if (child is T parent) return parent;
            child = VisualTreeHelper.GetParent(child);
        }
        return null;
    }

    private void DockAppContextMenu_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuItem { CommandParameter: DockApp app })
        {
            if (_contextTargetWasForeground && FindWindowForExecutable(app.ExecutablePath) is { } window)
                NativeWindowTracker.Activate(window.Handle);
            OpenDockApp(app, toggleIfForeground: _contextTargetWasForeground);
        }
    }

    private void PinDockApp_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuItem { CommandParameter: DockApp app })
        {
            return;
        }

        PinRunningApp(app);
    }

    internal void PinRunningApp(DockApp app, int insertIndex = -1)
    {
        if (RevealHiddenPin(app)) return;
        var (shortcutPath, error) = WindowsTaskbarPinImporter.AddPin(app.Name, app.ExecutablePath, app.LaunchArguments);
        if (error is not null || shortcutPath is null)
        {
            ReportError(error ?? "ErrorAppPinFailed");
            return;
        }

        var pinnedApp = new DockApp(app.Name, app.ExecutablePath)
        {
            Icon = app.Icon,
            LaunchArguments = app.LaunchArguments,
            ShortcutPath = shortcutPath,
            HasNotification = app.HasNotification,
            IsForeground = app.IsForeground,
            WindowCount = app.WindowCount
        };

        _runningApps.Remove(app);
        if (insertIndex >= 0 && insertIndex <= _pinnedApps.Count)
        {
            _pinnedApps.Insert(insertIndex, pinnedApp);
        }
        else
        {
            _pinnedApps.Add(pinnedApp);
        }

        SavePinOrder();
        ResizeDockToContent();
        _replacementSession?.RefreshPosition();
    }

    private void SavePinOrder()
    {
        var order = _pinnedApps.Select(app => app.ExecutablePath).ToList();
        var error = _preferences.Save(_preferences.Current with { PinOrder = order });
        if (error is not null) ReportError(error);
    }

    private void OnWidgetOrderChanged(IReadOnlyList<string> newOrder)
    {
        var error = _preferences.Save(_preferences.Current with { WidgetOrder = newOrder });
        if (error is not null) ReportError(error);
    }

    private void UnpinDockApp_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuItem { CommandParameter: DockApp { ShortcutPath: not null } app })
        {
            return;
        }

        var error = WindowsTaskbarPinImporter.RemovePin(app.ShortcutPath);
        if (error is not null)
        {
            ReportError(error);
            return;
        }

        AnimateDockAppRemoval(PinnedApps, _pinnedApps, app, () =>
        {
            SavePinOrder();
            if (_windowTracker is not null)
            {
                RefreshRunningApps();
            }

            ResizeDockToContent();
            _replacementSession?.RefreshPosition();
        });
    }

    private void CloseDockApp_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuItem { CommandParameter: string executablePath })
        {
            return;
        }

        CloseApplication(executablePath);
    }

    private void CloseApplication(string executablePath)
    {
        var (processes, inaccessibleCount) = FindProcessesForExecutable(executablePath);
        var closedCount = 0;
        var failedCount = inaccessibleCount;
        foreach (var process in processes)
        {
            try
            {
                if (process.CloseMainWindow())
                {
                    closedCount++;
                }
                else
                {
                    failedCount++;
                }
            }
            catch (InvalidOperationException)
            {
                // The process exited while the menu command was running.
            }
            catch (Win32Exception)
            {
                failedCount++;
            }
            finally
            {
                process.Dispose();
            }
        }

        if (closedCount == 0)
            ReportError("ErrorAppGracefulClose");
        else if (failedCount > 0)
            ReportError("ErrorAppPartialGracefulClose");
    }

    private void TerminateDockApp_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuItem { CommandParameter: string executablePath })
        {
            return;
        }

        TerminateApplication(executablePath);
    }

    private void TerminateApplication(string executablePath)
    {
        var (processes, inaccessibleCount) = FindProcessesForExecutable(executablePath);
        if (processes.Count == 0)
        {
            ReportError(inaccessibleCount == 0
                ? "ErrorAppNoProcesses"
                : "ErrorAppProcessesInaccessible");
            return;
        }

        var appName = _pinnedApps.FirstOrDefault(app =>
            string.Equals(app.ExecutablePath, executablePath, StringComparison.OrdinalIgnoreCase))?.Name
            ?? Path.GetFileNameWithoutExtension(executablePath);
        var confirmation = MessageBox.Show(
            UiText.Format("TerminatePrompt", appName, processes.Count),
            UiText.Get("TerminateTitle"),
            MessageBoxButton.YesNo,
            MessageBoxImage.Warning,
            MessageBoxResult.No);
        if (confirmation != MessageBoxResult.Yes)
        {
            DisposeProcesses(processes);
            return;
        }

        var terminatedCount = 0;
        var failedCount = inaccessibleCount;
        foreach (var process in processes)
        {
            try
            {
                if (process.Id == Environment.ProcessId)
                {
                    failedCount++;
                    continue;
                }

                process.Kill(entireProcessTree: true);
                terminatedCount++;
            }
            catch (InvalidOperationException)
            {
                // The process exited while the menu command was running.
            }
            catch (Win32Exception)
            {
                failedCount++;
            }
            finally
            {
                process.Dispose();
            }
        }

        if (terminatedCount == 0)
            ReportError("ErrorAppTerminateFailed");
        else if (failedCount > 0)
            ReportError("ErrorAppPartialTerminate");
    }

    internal void OpenSettingsFromSecondaryDock() => OpenSettings_Click(this, new RoutedEventArgs());

    private void OpenSettings_Click(object sender, RoutedEventArgs e)
    {
        _widgetPanel.Close(restoreFocus: false);
        AppLog.Info($"Settings open requested: source={sender?.GetType().Name ?? "null"}, startupArg={((App)Application.Current).OpenSettingsOnStartup}, existing={(_settingsWindow is null ? "none" : _settingsWindow.IsClosingOrClosed ? "closing" : "open")}");
        if (_settingsWindow is { IsClosingOrClosed: true }) _settingsWindow = null;
        if (_settingsWindow is null)
        {
            var settingsWindow = new SettingsWindow(_preferences);
            _settingsWindow = settingsWindow;
            settingsWindow.Activated += (_, _) => _autoHideController?.EvaluateState();
            settingsWindow.Deactivated += (_, _) => _autoHideController?.EvaluateState();
            settingsWindow.Closed += (_, _) =>
            {
                if (ReferenceEquals(_settingsWindow, settingsWindow)) _settingsWindow = null;
                if (_autoHideController is not null)
                {
                    _autoHideController.CheckForegroundWindow(NativeWindowTracker.GetForegroundApplicationWindow());
                    _autoHideController.EvaluateState();
                }
                NativeMemoryMethods.TrimWorkingSet(force: true);
            };
            _settingsWindow.Show();
        }
        if (_settingsWindow.WindowState == WindowState.Minimized) _settingsWindow.WindowState = WindowState.Normal;
        _settingsWindow.Activate();
    }

    private System.Windows.Media.Imaging.BitmapSource? ApplyApplicationIcon(DockApp app)
    {
        // Pinned apps need IconLocation (Chrome/Edge PWA .ico) or AUMID (Store apps); the
        // executable alone has no icon resource for those. GetForPinnedApp tries both before
        // falling back to the executable's own icon.
        var raw = ApplicationIconProvider.GetForPinnedApp(app.ExecutablePath, app.IconLocation, app.AppUserModelId);
        return ThemePacks.ApplyIcon(raw, app.ExecutablePath);
    }

    private void ThemePacks_Changed(object? sender, EventArgs e)
    {
        RefreshRecycleBinIcon();
        ApplyStartIcon();
        foreach (var app in _pinnedApps)
        {
            app.Icon = ApplyApplicationIcon(app);
        }
        if (_windowTracker is not null)
        {
            WindowTracker_WindowsChanged(this, _windowTracker.Snapshot);
        }
    }

    // The theme's @start image replaces the Windows logo; without one the authentic native Windows logo is shown.
    private void ApplyStartIcon()
    {
        var image = ThemePacks.ApplyIcon(null, null, specialKey: IconPack.StartKey)
                    ?? ThemePacks.GetNativeWindowsLogo();
        StartImage.Source = image;
        StartImage.Visibility = image is null ? Visibility.Collapsed : Visibility.Visible;
        StartLogo.Visibility = image is null ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>
    /// Applies the configured width mode. A fixed width reuses the manual-resize pin so
    /// content fitting never grows past it; resize thumbs are hidden in fixed modes.
    /// </summary>
    private void ApplyDockWidthMode()
    {
        var settings = _preferences.Current;
        var fixedMode = settings.DockWidthMode is DockWidthMode.Pixels or DockWidthMode.Percent;
        LeftResizeThumb.Visibility = IsVertical || fixedMode || IsEdgeToEdge ? Visibility.Collapsed : Visibility.Visible;
        RightResizeThumb.Visibility = IsVertical || fixedMode || IsEdgeToEdge ? Visibility.Collapsed : Visibility.Visible;

        if (!fixedMode || IsEdgeToEdge)
        {
            _dockWidthOverride = null;
            return;
        }

        var target = DisplayService.GetTargetDisplay(settings.TargetDisplay);
        var monitorExtent = IsVertical
            ? (target.Height > 0 ? target.Height : SystemParameters.WorkArea.Height)
            : (target.Width > 0 ? target.Width : SystemParameters.WorkArea.Width);
        var length = settings.DockWidthMode == DockWidthMode.Pixels
            ? settings.DockWidthPixels
            : monitorExtent * settings.DockWidthPercent / 100.0;
        _dockWidthOverride = Math.Max(MinimumDockSurfaceWidth, Math.Min(length, monitorExtent));
    }

    private void Preferences_Changed(object? sender, EventArgs e)
    {
        _sizePreviewOverride = null;
        _radiusPreviewOverride = null;
        // Clear any live-slider deferral so ApplySizeProfile commits the updated AppBar shelf immediately.
        if (_replacementSession is not null) _replacementSession.DeferReservation = false;
        ResetMagnification(immediate: true);
        DockSurfaceTranslate.X = 0;
        ThemePacks_Changed(this, EventArgs.Empty);
        _dockPosition = _preferences.Current.Position;
        _thumbnailPreview.AnimationsEnabled = _preferences.Current.EffectiveAnimationsEnabled;
        if (_autoHideController is not null) _autoHideController.AnimationsEnabled = _preferences.Current.EffectiveAnimationsEnabled;
        _autoHideController?.SetMode(_preferences.Current.AutoHide);
        _replacementSession?.UpdateAutoHide(_preferences.Current.AutoHide != AutoHideMode.Disabled);
        ApplyBackdropHitTesting();
        _widgetCoordinator.Apply(_preferences.Current);
        ApplyWidgets(_preferences.Current.Widgets);
        ApplyExtrasSettings();
        ApplyFeatureSettings();
        ApplyHeadroom();
        ApplySizeProfile();
        ApplyLayoutMode();
        ApplyDockWidthMode();
        ResizeDockToContent();
        _thumbnailPreview.Hide();
        if (_openContextMenu is not null) _openContextMenu.IsOpen = false;
        ResetMagnification();
        if (_windowTracker is not null)
        {
            var states = RunningWindowStates();
            UpdateNotificationBadges(_pinnedApps, states);
            UpdateNotificationBadges(_runningApps, states);
        }
        var targetDisplay = DisplayService.GetTargetDisplay(_preferences.Current.TargetDisplay);
        if (_replacementSession?.IsActive == true)
        {
            _replacementSession.SetTargetMonitor(targetDisplay.Handle);
            ApplyDockAlignment();
        }
        else
        {
            PositionDock();
        }

        var geometry = (
            Size: _preferences.Current.Size,
            Scale: _preferences.Current.DockScale,
            Padding: _preferences.Current.DockIconPadding,
            BottomGap: ExternalBottomGap,
            TopGap: ExternalTopGap);
        if (_appliedDockGeometry is { } applied && applied != geometry && _replacementSession is not null)
        {
            if (applied.Size != geometry.Size || Math.Abs(applied.Scale - geometry.Scale) > 0.001 || applied.Padding != geometry.Padding)
            {
                AnimateDockScaleChange();
            }
            // Commit the updated work area in place (re-registering only if Explorer kept stale rcWork)
            // without ever showing or flashing the native Windows taskbar (#86, #92).
            _replacementSession.EnsureReservationSynced();
        }
        _appliedDockGeometry = geometry;
    }

    /// <summary>Subtle settle animation when a new size/scale profile is committed.</summary>
    private void AnimateDockScaleChange()
    {
        if (!_preferences.Current.EffectiveAnimationsEnabled) return;
        DockScaleTransform.BeginAnimation(ScaleTransform.ScaleXProperty, null);
        DockScaleTransform.BeginAnimation(ScaleTransform.ScaleYProperty, null);
        var animation = new DoubleAnimation(0.94, 1.0, TimeSpan.FromMilliseconds(220))
        {
            EasingFunction = new BackEase { Amplitude = 0.35, EasingMode = EasingMode.EaseOut }
        };
        DockScaleTransform.BeginAnimation(ScaleTransform.ScaleXProperty, animation);
        DockScaleTransform.BeginAnimation(ScaleTransform.ScaleYProperty, animation);
    }

    private void RemoveDockWidget(string id)
    {
        var settings = _preferences.Current;
        var widgets = (settings.Widgets ?? [])
            .Where(widget => !string.Equals(widget, id, StringComparison.OrdinalIgnoreCase))
            .ToArray();
        var installed = (settings.InstalledWidgets ?? [])
            .Where(widget => !string.Equals(widget, id, StringComparison.OrdinalIgnoreCase))
            .ToArray();
        var order = settings.WidgetOrder?
            .Where(widget => !string.Equals(widget, id, StringComparison.OrdinalIgnoreCase))
            .ToArray();
        var dockItemOrder = settings.DockItemOrder?
            .Where(token => !string.Equals(token, "widget:" + id, StringComparison.OrdinalIgnoreCase))
            .ToArray();
        if (widgets.Length == (settings.Widgets?.Count ?? 0) && installed.Length == (settings.InstalledWidgets?.Count ?? 0)) return;

        // Close the gap immediately instead of animating the dock width after a removal.
        _instantDockResize = true;
        try
        {
            var updated = settings with
            {
                Widgets = widgets,
                InstalledWidgets = installed,
                WidgetOrder = order,
                DockItemOrder = dockItemOrder,
                ShowClock = id.Equals("Clock", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowClock,
                ShowMedia = id.Equals("Media", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowMedia,
                ShowSystemStats = id.Equals("SystemStats", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowSystemStats,
                ShowFocus = id.Equals("Focus", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowFocus,
                ShowAudio = id.Equals("Audio", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowAudio,
                ShowQuickSettings = id.Equals("QuickSettings", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowQuickSettings,
                ShowWeather = id.Equals("Weather", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowWeather
            };
            var error = _preferences.Save(updated);
            if (error is not null) ReportError(error);
            _widgetCoordinator.Apply(updated);
            ApplyWidgets(updated.Widgets);
            RebuildDockOrder();
            ResizeDockToContent();
            _replacementSession?.RefreshPosition();
        }
        finally
        {
            _instantDockResize = false;
        }
    }

    private void ApplyWidgets(IReadOnlyList<string>? selected)
    {
        _widgetPanel.Close(restoreFocus: false);
        foreach (var timed in _widgets.OfType<ITimedDockWidget>()) timed.PropertyChanged -= TimedWidget_PropertyChanged;
        _widgets.Clear();
        var effectiveSelected = _preferences.Current.FocusModeEnabled ? null : selected;
        var clockStripActive = effectiveSelected?.Contains(DockWidgetService.ClockId, StringComparer.OrdinalIgnoreCase) == true;
        var statsStripActive = effectiveSelected?.Contains(DockWidgetService.SystemStatsId, StringComparer.OrdinalIgnoreCase) == true;
        var filtered = effectiveSelected?.Where(id =>
            !WidgetCoordinator.HandlesWidget(id)
            && (!clockStripActive || id != DockWidgetService.DateId)
            && (!statsStripActive || id != DockWidgetService.BatteryId))
            .ToArray();
        foreach (var widget in _widgetService.CreateWidgets(filtered)) _widgets.Add(widget);
        foreach (var timed in _widgets.OfType<ITimedDockWidget>()) timed.PropertyChanged += TimedWidget_PropertyChanged;

        var activeIds = new HashSet<string>(_widgets.Select(w => w.Id), StringComparer.OrdinalIgnoreCase);
        var removedKeys = _legacyWidgetElements.Keys.Where(k => !activeIds.Contains(k)).ToArray();
        foreach (var key in removedKeys)
        {
            _legacyWidgetElements.Remove(key);
        }

        WidgetDivider.Visibility = Visibility.Collapsed;
        DockWidgets.Visibility = Visibility.Collapsed;
        if (!_preferences.Current.FocusModeEnabled && (_widgets.Count > 0 || _widgetCoordinator.HasActiveWidgets))
        {
            DockWidgetService.Refresh(_widgets, _preferences.Current.Language);
            if (!_widgetRefreshTimer.IsEnabled) _widgetRefreshTimer.Start();
        }
        else
        {
            _widgetRefreshTimer.Stop();
        }
        UpdateTimedWidgetRefresh();
        RebuildDockOrder();
        ResizeDockToContent();
    }

    /// <summary>Free-form order tokens owned by permanent shell buttons (draggable via ShellDockDragData).</summary>
    internal static readonly string[] ShellDockTokens = ["start", "search", "taskview", "tray", "peek", "bin"];

    private IReadOnlyList<string> DefaultDockOrder(DockSettings settings)
    {
        var tokens = new List<string>();
        if (settings.StartPlacement != StartButtonPlacement.Hidden) tokens.Add("start");
        if (settings.ShowSearchButton) tokens.Add("search");
        if (settings.ShowTaskViewButton) tokens.Add("taskview");
        if (settings.ShowDesktopButton) tokens.Add("peek");

        var groupedPaths = (settings.AppGroups ?? []).SelectMany(g => g.ExecutablePaths)
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        var pins = (settings.PinOrder ?? _pinnedApps.Select(a => a.ExecutablePath).Where(p => p is not null).ToArray())
            .Where(p => p is not null).ToArray();
        foreach (var path in pins)
        {
            if (!groupedPaths.Contains(path)) tokens.Add("app:" + path);
        }
        foreach (var app in _runningApps)
        {
            if (!pins.Contains(app.ExecutablePath, StringComparer.OrdinalIgnoreCase) && !groupedPaths.Contains(app.ExecutablePath))
                tokens.Add("app:" + app.ExecutablePath);
        }
        foreach (var group in settings.AppGroups ?? []) tokens.Add("group:" + group.Id);
        foreach (var stack in _stacks) tokens.Add("stack:" + stack.Path);
        if (settings.ShowRecycleBin) tokens.Add("bin");
        foreach (var id in _widgetCoordinator.CurrentOrder)
        {
            if (_widgetCoordinator.ActiveWidgets.ContainsKey(id)) tokens.Add("widget:" + id);
        }
        foreach (var legacy in _widgets)
        {
            var token = "widget:" + legacy.Id;
            if (!tokens.Contains(token, StringComparer.OrdinalIgnoreCase))
            {
                tokens.Add(token);
            }
        }
        // Background apps is pinned to the far right and re-applied by DockItemOrdering on every rebuild.
        if (settings.ShowTrayButton) tokens.Add("tray");
        return tokens;
    }

    private DockApp? FindDockApp(string path) => _pinnedApps.Concat(_runningApps)
        .FirstOrDefault(app => string.Equals(app.ExecutablePath, path, StringComparison.OrdinalIgnoreCase));

    private FrameworkElement? CreateDockItemElement(string token)
    {
        if (string.Equals(token, "start", StringComparison.OrdinalIgnoreCase)) return StartButton;
        if (string.Equals(token, "search", StringComparison.OrdinalIgnoreCase))
            return _preferences.Current.ShowSearchButton ? SearchButton : null;
        if (string.Equals(token, "taskview", StringComparison.OrdinalIgnoreCase))
            return _preferences.Current.ShowTaskViewButton ? TaskViewButton : null;
        if (string.Equals(token, "tray", StringComparison.OrdinalIgnoreCase))
            return _preferences.Current.ShowTrayButton ? TrayButton : null;
        if (string.Equals(token, "peek", StringComparison.OrdinalIgnoreCase))
            return _preferences.Current.ShowDesktopButton ? ShowDesktopButton : null;
        if (string.Equals(token, "bin", StringComparison.OrdinalIgnoreCase)) return RecycleBinButton;
        if (token.StartsWith("app:", StringComparison.OrdinalIgnoreCase))
        {
            var app = FindDockApp(token[4..]);
            if (app is null || GroupContaining(app.ExecutablePath) is not null) return null;
            var element = (FrameworkElement)((DataTemplate)FindResource("DockAppIconTemplate")).LoadContent();
            element.DataContext = app;
            return element;
        }
        if (token.StartsWith("group:", StringComparison.OrdinalIgnoreCase))
        {
            var group = FindAppGroup(token[6..]);
            if (group is null) return null;
            var element = (FrameworkElement)((DataTemplate)FindResource("DockGroupIconTemplate")).LoadContent();
            element.DataContext = CreateGroupViewModel(group);
            return element;
        }
        if (token.StartsWith("stack:", StringComparison.OrdinalIgnoreCase))
        {
            var stack = _stacks.FirstOrDefault(s => string.Equals(s.Path, token[6..], StringComparison.OrdinalIgnoreCase));
            if (stack is null) return null;
            var element = (FrameworkElement)((DataTemplate)FindResource("StackIconTemplate")).LoadContent();
            element.DataContext = stack;
            return element;
        }
        if (token.StartsWith("widget:", StringComparison.OrdinalIgnoreCase))
        {
            var widgetId = token[7..];
            if (_widgetCoordinator.ActiveWidgets.TryGetValue(widgetId, out var widget))
            {
                return widget;
            }
            if (GetOrCreateLegacyWidgetElement(widgetId) is { } legacyElement)
            {
                return legacyElement;
            }
        }
        return null;
    }

    private FrameworkElement? GetOrCreateLegacyWidgetElement(string widgetId)
    {
        var widgetItem = _widgets.FirstOrDefault(w => string.Equals(w.Id, widgetId, StringComparison.OrdinalIgnoreCase));
        if (widgetItem is null) return null;

        if (_legacyWidgetElements.TryGetValue(widgetId, out var existing))
        {
            existing.Content = widgetItem;
            existing.DataContext = widgetItem;
            return existing;
        }

        var host = new ContentControl
        {
            Content = widgetItem,
            DataContext = widgetItem,
            Focusable = false,
            Tag = widgetId,
            VerticalAlignment = VerticalAlignment.Center,
            ContextMenu = (ContextMenu)FindResource("DockContextMenu")
        };
        host.SizeChanged += (_, e) =>
        {
            if (e.WidthChanged) ResizeDockToContent();
        };
        host.PreviewMouseLeftButtonDown += LegacyWidget_PreviewMouseLeftButtonDown;
        host.PreviewMouseLeftButtonUp += LegacyWidget_PreviewMouseLeftButtonUp;
        host.PreviewMouseMove += LegacyWidget_PreviewMouseMove;

        _legacyWidgetElements[widgetId] = host;
        return host;
    }

    private void LegacyWidget_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is DependencyObject dep && (WidgetCoordinator.IsInteractiveControl(dep) || FindVisualParent<ButtonBase>(dep) is not null))
        {
            _draggedLegacyWidget = null;
            return;
        }
        if (sender is ContentControl host)
        {
            _legacyDragStartPoint = e.GetPosition(DockContents);
            _draggedLegacyWidget = host;
        }
    }

    private void LegacyWidget_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        _draggedLegacyWidget = null;
    }

    private void LegacyWidget_PreviewMouseMove(object sender, MouseEventArgs e)
    {
        if (e.LeftButton != MouseButtonState.Pressed || _draggedLegacyWidget is null || sender is not ContentControl host)
        {
            return;
        }
        var currentPoint = e.GetPosition(DockContents);
        var diff = _legacyDragStartPoint - currentPoint;
        if (Math.Abs(diff.X) > SystemParameters.MinimumHorizontalDragDistance
            || Math.Abs(diff.Y) > SystemParameters.MinimumVerticalDragDistance)
        {
            var target = _draggedLegacyWidget;
            _draggedLegacyWidget = null;
            if (target.Tag is string widgetId)
            {
                if (Mouse.Captured is UIElement captured) captured.ReleaseMouseCapture();
                DragGhost.DoDragDrop(target, new WidgetCoordinator.WidgetDragPayload(widgetId), DragDropEffects.Move);
            }
        }
    }

    // Keep the saved order and newly discovered apps in one consistent sequence.
    // Both drawing and drag/drop must use the same effective order.
    private List<string> BuildEffectiveDockOrder()
    {
        var settings = _preferences.Current;
        var tokens = (settings.DockItemOrder ?? DefaultDockOrder(settings)).ToList();
        foreach (var token in DefaultDockOrder(settings))
        {
            if (!tokens.Contains(token, StringComparer.OrdinalIgnoreCase))
            {
                if (token.StartsWith("app:", StringComparison.OrdinalIgnoreCase))
                {
                    var lastAppIndex = tokens.FindLastIndex(t =>
                        t.StartsWith("app:", StringComparison.OrdinalIgnoreCase)
                        && FindDockApp(t[4..]) is not null);

                    if (lastAppIndex >= 0)
                    {
                        tokens.Insert(lastAppIndex + 1, token);
                    }
                    else
                    {
                        var startIndex = tokens.FindIndex(t => string.Equals(t, "start", StringComparison.OrdinalIgnoreCase));
                        if (startIndex >= 0)
                        {
                            tokens.Insert(startIndex + 1, token);
                        }
                        else
                        {
                            tokens.Insert(0, token);
                        }
                    }
                }
                else if (string.Equals(token, "peek", StringComparison.OrdinalIgnoreCase))
                {
                    // Orders saved before Show Desktop existed keep it beside Start instead of appending it.
                    var startIndex = tokens.FindIndex(t => string.Equals(t, "start", StringComparison.OrdinalIgnoreCase));
                    var searchIndex = tokens.FindIndex(t => string.Equals(t, "search", StringComparison.OrdinalIgnoreCase));
                    var insertAt = startIndex >= 0 ? startIndex + 1 : 0;
                    if (searchIndex >= 0) insertAt = searchIndex + 1;
                    tokens.Insert(Math.Min(insertAt, tokens.Count), token);
                }
                else
                {
                    tokens.Add(token);
                }
            }
        }

        // The Windows-native group and the background-apps group ignore the persisted order entirely.
        return DockItemOrdering.ApplyPinnedGroups(tokens, settings).ToList();
    }

    /// <summary>Rebuilds the single dock strip from the persisted free-form item order.</summary>
    private void RebuildDockOrder()
    {
        if (_rebuildingDockOrder || DockContents is null) return;
        _rebuildingDockOrder = true;
        ResetMagnification(immediate: true);
        try
        {
            var tokens = BuildEffectiveDockOrder();
            var pairs = new List<(string Token, FrameworkElement Element)>();
            foreach (var token in tokens.Distinct(StringComparer.OrdinalIgnoreCase))
            {
                // Horizontal widgets are hidden on narrow vertical side docks so they never widen or clip the strip.
                if ((IsVertical || _preferences.Current.FocusModeEnabled) && token.StartsWith("widget:", StringComparison.OrdinalIgnoreCase)) continue;
                if (CreateDockItemElement(token) is { } element) pairs.Add((token, element));
            }

            foreach (var element in _dockItemElements.Values) DockContents.Children.Remove(element);
            _dockItemElements.Clear();
            DockContents.Children.Remove(StartDivider);
            DockContents.Children.Remove(TrayGroupDivider);

            var insertAt = DockContents.Children.IndexOf(StatusText);
            if (insertAt < 0) insertAt = DockContents.Children.Count;

            var leadCount = pairs.Count(pair => DockItemOrdering.PinnedLeadTokens.Contains(pair.Token, StringComparer.OrdinalIgnoreCase));
            var trayIndex = pairs.FindIndex(pair => string.Equals(pair.Token, "tray", StringComparison.OrdinalIgnoreCase));
            // The shell group always gets its hairline; when Tray follows immediately its own divider avoids a double line.
            var showLeadDivider = leadCount > 0 && trayIndex != leadCount;
            var showTrayDivider = trayIndex > 0;

            for (var i = 0; i < pairs.Count; i++)
            {
                if (i == leadCount && showLeadDivider)
                    DockContents.Children.Insert(insertAt++, StartDivider);
                if (i == trayIndex && showTrayDivider)
                    DockContents.Children.Insert(insertAt++, TrayGroupDivider);

                var (token, element) = pairs[i];
                // A UIElement cannot be a logical child of two panels; detach from the old grouped host first.
                if (element.Parent is Panel oldPanel) oldPanel.Children.Remove(element);
                DockContents.Children.Insert(insertAt++, element);
                _dockItemElements[token] = element;
            }
            if (showLeadDivider && leadCount >= pairs.Count)
                DockContents.Children.Insert(insertAt++, StartDivider);

            StartDivider.Visibility = showLeadDivider ? Visibility.Visible : Visibility.Collapsed;
            TrayGroupDivider.Visibility = showTrayDivider ? Visibility.Visible : Visibility.Collapsed;
            InvalidateMagnificationTargets();
        }
        finally
        {
            _rebuildingDockOrder = false;
        }
    }

    /// <summary>Moves a dock item to an absolute position and persists the new order.</summary>
    private void MoveDockItem(string token, int index)
    {
        var settings = _preferences.Current;
        var visibleOrder = VisibleDockItems().Select(item => item.Key).ToArray();
        var oldOrder = BuildEffectiveDockOrder();
        var tokens = DockItemOrdering.MoveVisibleItem(oldOrder, visibleOrder, token, index);
        if (tokens.SequenceEqual(oldOrder, StringComparer.OrdinalIgnoreCase)) return;
        var error = _preferences.Save(settings with { DockItemOrder = tokens });
        if (error is not null)
        {
            ReportError(error);
            return;
        }
        RebuildDockOrder();
        ResizeDockToContent();
        _replacementSession?.RefreshPosition();
    }

    private int _memoryTrimCounter;

    private void WidgetRefreshTimer_Tick(object? sender, EventArgs e)
    {
        DockWidgetService.Refresh(_widgets, _preferences.Current.Language);
        ResizeDockToContent();
        if (++_memoryTrimCounter >= 3)
        {
            _memoryTrimCounter = 0;
            NativeMemoryMethods.TrimWorkingSet();
        }
    }

    private void DockWidgets_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (e.WidthChanged) ResizeDockToContent();
    }

    private void TimedWidget_DetailsRequested(object sender, RoutedEventArgs e)
    {
        if (e.OriginalSource is not TimedWidgetTile { DataContext: DockWidgetItem widget } tile) return;
        e.Handled = true;
        _widgetPanel.Open(tile, widget is CountdownDockWidget ? UiText.Get("WidgetCountdown") : UiText.Get("WidgetStopwatch"),
            new TimedWidgetPanel { DataContext = widget });
    }

    private void WidgetPanel_IsOpenChanged(object? sender, EventArgs e)
    {
        if (_widgetPanel.IsOpen)
        {
            _previewCloseTimer.Stop();
            _thumbnailPreview.Hide();
            if (_openContextMenu is not null) _openContextMenu.IsOpen = false;
            ResetMagnification();
        }
        _autoHideController?.EvaluateState();
    }

    private void TimedWidget_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ITimedDockWidget.IsRunning)) UpdateTimedWidgetRefresh();
    }

    private void UpdateTimedWidgetRefresh()
    {
        if (_widgets.OfType<ITimedDockWidget>().Any(widget => widget.IsRunning)) _timedWidgetRefreshTimer.Start();
        else _timedWidgetRefreshTimer.Stop();
    }

    private void TimedWidgetRefreshTimer_Tick(object? sender, EventArgs e)
    {
        foreach (var widget in _widgets.OfType<ITimedDockWidget>()) widget.Refresh();
    }

    private static MenuItem? FindMenuItem(ItemsControl owner, string tag)
    {
        foreach (var item in owner.Items.OfType<MenuItem>())
        {
            if (string.Equals(item.Tag as string, tag, StringComparison.Ordinal))
            {
                return item;
            }

            var nested = FindMenuItem(item, tag);
            if (nested is not null)
            {
                return nested;
            }
        }

        return null;
    }

    private static void SetMenuItemVisibility(ItemsControl owner, string tag, bool visible)
    {
        var item = FindMenuItem(owner, tag);
        if (item is not null)
        {
            item.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        }
    }

    private static void SetSeparatorVisibility(ItemsControl owner, string tag, bool visible)
    {
        var separator = owner.Items.OfType<Separator>()
            .FirstOrDefault(item => string.Equals(item.Tag as string, tag, StringComparison.Ordinal));
        if (separator is not null)
        {
            separator.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        }
    }

    private static void SetMenuCommandParameter(ItemsControl owner, string? executablePath)
    {
        foreach (var item in owner.Items.OfType<MenuItem>())
        {
            item.CommandParameter = executablePath;
            SetMenuCommandParameter(item, executablePath);
        }
    }

    private static (List<Process> Processes, int InaccessibleCount) FindProcessesForExecutable(string executablePath)
    {
        var matchingProcesses = new List<Process>();
        var inaccessibleCount = 0;
        var processName = Path.GetFileNameWithoutExtension(executablePath);
        foreach (var process in Process.GetProcessesByName(processName))
        {
            try
            {
                if (string.Equals(process.MainModule?.FileName, executablePath, StringComparison.OrdinalIgnoreCase))
                {
                    matchingProcesses.Add(process);
                }
                else
                {
                    process.Dispose();
                }
            }
            catch (Win32Exception)
            {
                inaccessibleCount++;
                process.Dispose();
            }
            catch (InvalidOperationException)
            {
                process.Dispose();
            }
        }

        return (matchingProcesses, inaccessibleCount);
    }

    private static void DisposeProcesses(IEnumerable<Process> processes)
    {
        foreach (var process in processes)
        {
            process.Dispose();
        }
    }

    private bool IsEdgeToEdge => _preferences.Current.DockWidthMode == DockWidthMode.Fill;
    private bool IsFullWidth => _preferences.Current.Layout == DockLayout.FullWidth || IsEdgeToEdge;

    private void ApplyLayoutMode()
    {
        if (IsVertical)
        {
            if (DockLayoutRoot.RowDefinitions.Count > 1)
                DockLayoutRoot.RowDefinitions[1].Height = IsFullWidth ? new GridLength(1, GridUnitType.Star) : GridLength.Auto;
        }
        else
        {
            AppsColumn.Width = IsFullWidth ? GridLength.Auto : new GridLength(1, GridUnitType.Star);
            WidgetColumn.Width = IsFullWidth ? new GridLength(1, GridUnitType.Star) : GridLength.Auto;
            WidgetIsland.HorizontalAlignment = IsFullWidth ? HorizontalAlignment.Right : HorizontalAlignment.Left;
        }
        ApplyDockMargin();
        ApplyDockAlignment();
        ResizeDockToContent();
    }

    /// <summary>
    /// Computes the reserved shelf height (DIP) covering the dock surface (including border thickness so Compact
    /// buttons never receive a WPF LayoutClip, #81), the external bottom gap, and the external top gap (#92).
    /// </summary>
    internal static double ComputeEffectiveShelfHeight(
        DockSizeProfile profile, bool isVertical, double borderThickness, double externalBottomGap, double externalTopGap)
    {
        var border = Math.Max(0.0, borderThickness) * 2.0;
        // Use the padding-free cell height: horizontal icon spacing must never grow the reserved taskbar strip.
        var requiredSurface = profile.EffectiveCellHeight + profile.VerticalPadding * 2 + border;
        var topGap = Math.Max(0.0, externalTopGap);
        var bottomGap = Math.Max(0.0, externalBottomGap);
        if (isVertical)
            return Math.Max(profile.Shelf + 16, requiredSurface + bottomGap + 16) + topGap;
        return Math.Max(profile.Shelf, requiredSurface + bottomGap + 2) + topGap;
    }

    private double EffectiveShelfHeight
    {
        get
        {
            var borderThickness = _preferences.Current.DockBackground == SurfaceBackground.None
                ? 0.0
                : _preferences.Current.DockBorderThickness;
            return ComputeEffectiveShelfHeight(Profile, IsVertical, borderThickness, ExternalBottomGap, ExternalTopGap);
        }
    }

    /// <summary>
    /// Transparent, click-through headroom above the reserved shelf so launch-bounce and hover magnification
    /// animations draw above neighbouring app windows instead of clipping at the top edge of the dock window.
    /// ExternalTopGap is reserved inside <see cref="EffectiveShelfHeight"/> so maximized windows respect it (#92).
    /// </summary>
    private double EffectiveHeadroom => DockMetrics.Headroom;

    /// <summary>External spacing above the visible bar, resolved from the user's pixels/percent preference.</summary>
    private double ExternalTopGap
    {
        get
        {
            var settings = _preferences.Current;
            var target = DisplayService.GetTargetDisplay(settings.TargetDisplay);
            var extent = IsVertical ? target.Height : target.Width;
            if (extent <= 0) extent = IsVertical ? SystemParameters.WorkArea.Height : SystemParameters.WorkArea.Width;
            return DockMetrics.ResolveGap(settings.DockTopGapUnit, settings.DockTopGapPixels, settings.DockTopGapPercent, extent);
        }
    }

    /// <summary>External spacing below the visible bar, resolved from the user's pixels/percent preference.</summary>
    private double ExternalBottomGap
    {
        get
        {
            var settings = _preferences.Current;
            var target = DisplayService.GetTargetDisplay(settings.TargetDisplay);
            var extent = IsVertical ? target.Height : target.Width;
            if (extent <= 0) extent = IsVertical ? SystemParameters.WorkArea.Height : SystemParameters.WorkArea.Width;
            return DockMetrics.ResolveGap(settings.DockBottomGapUnit, settings.DockBottomGapPixels, settings.DockBottomGapPercent, extent);
        }
    }

    private void ApplyDockMargin()
    {
        // Edge-to-edge fill removes the side gaps entirely (Windows-style full-width taskbar); regular full-width
        // keeps its small inset so the rounded surface does not clip against the screen edge.
        var side = IsEdgeToEdge ? 0 : IsFullWidth ? DockMetrics.FullWidthSideInset : DockMetrics.SideInset;
        var bottomGap = ExternalBottomGap;
        // The headroom row/column already supplies the external top gap; the surface only needs its bottom spacing.
        DockSurface.Margin = Edge switch
        {
            DockEdge.Left => new Thickness(bottomGap, side, 0, side),
            DockEdge.Right => new Thickness(0, side, bottomGap, side),
            _ => new Thickness(side, 0, side, bottomGap)
        };
    }

    private double EffectiveCornerRadius => IsEdgeToEdge && _radiusPreviewOverride is null
        ? 0 // Windows-style edge-to-edge fill: square corners flush with the screen edges.
        : _radiusPreviewOverride ?? (_preferences.Current.DockCornerRadius is { } customRadius && customRadius >= 0
            ? customRadius
            : Profile.Radius);

    private DockSizeProfile Profile => _sizePreviewOverride is { } preview
        ? DockMetrics.For(preview.Size, preview.Scale, preview.Padding)
        : DockMetrics.For(_preferences.Current.Size, _preferences.Current.DockScale, _preferences.Current.DockIconPadding);

    internal void SetSizePreview(DockSize? size, double? scale, int? padding = null)
    {
        _sizePreviewOverride = size is { } s && scale is { } sc
            ? (s, sc, padding ?? _preferences.Current.DockIconPadding)
            : null;
        if (_replacementSession is not null) _replacementSession.DeferReservation = _sizePreviewOverride is not null;
        ApplySizeProfile();
        if (_replacementSession?.IsActive == true)
        {
            var thickness = IsVertical ? _dockArea.Width : _dockArea.Height;
            if (thickness > 0) _dockTopGap = Math.Max(0.0, thickness - Profile.SurfaceHeight - Profile.BottomGap);
        }
        ApplyDockMargin();
        ResizeDockToContent();
        _replacementSession?.RefreshPosition();
    }

    internal void SetGeometryPreview(int? radius)
    {
        _radiusPreviewOverride = radius;
        ApplySizeProfile();
        ApplyDockMargin();
        ResizeDockToContent();
        _replacementSession?.RefreshPosition();
    }

    private void ApplySizeProfile()
    {
        UpdatePreferredIconPixels();
        var profile = Profile;
        var radius = EffectiveCornerRadius;
        // Flyouts read this gap so their spacing from the dock follows the adjustable dock height/scale.
        DockMetrics.FlyoutGap = DockMetrics.ComputeFlyoutGap(profile);
        // One master radius drives every nested corner in the application.
        RadiusManager.Apply((int)Math.Round(radius));
        Application.Current.Resources["DockCellSize"] = profile.Cell;
        // Height is padding-free so extra horizontal spacing can never grow icons or dock height.
        Application.Current.Resources["DockCellHeight"] = profile.EffectiveCellHeight;
        // Horizontal-only padding: left/right gap between icons, top/bottom stays at zero.
        Application.Current.Resources["DockIconPaddingResource"] = new Thickness(profile.IconPadding, 0, profile.IconPadding, 0);
        ApplyDividerPadding(profile.IconPadding);
        Application.Current.Resources["DockIconSize"] = profile.Icon;
        Application.Current.Resources["DockUtilityGlyphSize"] = Math.Round(profile.Icon * 0.6);
        Application.Current.Resources["StartLogoSize"] = Application.Current.Resources["DockApplicationIconSize"] is double currentSize ? currentSize : 36.0;
        DockFrame.Padding = new Thickness(10, profile.VerticalPadding, 10, profile.VerticalPadding);
        DockFrame.CornerRadius = new CornerRadius(radius);
        DockInnerBevel.CornerRadius = new CornerRadius(Math.Max(0, radius - 1));
        DockScrollViewer.ExternalIndicatorCornerRadius = Math.Max(0, radius - 1);
        DockSheen.CornerRadius = new CornerRadius(radius);
        DockSheen.Margin = new Thickness(-10, -profile.VerticalPadding, -10, -profile.VerticalPadding);
        DockDynamicGlintBorder.CornerRadius = new CornerRadius(radius, radius, 0, 0);
        DockDynamicGlowBorder.CornerRadius = new CornerRadius(radius, radius, 0, 0);
        DockDynamicGlintBorder.Margin = new Thickness(-10, -profile.VerticalPadding, -10, 0);
        DockDynamicGlowBorder.Margin = new Thickness(-10, -profile.VerticalPadding, -10, 0);
        var bevelH = -9;
        var bevelV = -profile.VerticalPadding + 1;
        DockInnerBevel.Margin = IsVertical ? new Thickness(bevelV, bevelH, bevelV, bevelH) : new Thickness(bevelH, bevelV, bevelH, bevelV);
        if (IsVertical) DockFrame.Padding = new Thickness(profile.VerticalPadding + 4, 10, profile.VerticalPadding + 4, 10);
        var scale = profile.WidgetScale;
        var widgetTransform = Math.Abs(scale - 1) > 0.001 ? new ScaleTransform(scale, scale) : Transform.Identity;
        WidgetStrip.LayoutTransform = widgetTransform;
        DockWidgets.LayoutTransform = widgetTransform;
        // The external bottom gap is subtracted from the shelf by DockSurface.Margin, so the frame's inner
        // height must still fit the full DockCellHeight. Otherwise WPF gives every DockButton a LayoutClip
        // (e.g. 0,0,38,36) and magnified icons get cut at the top edge of the dock. Grow the shelf instead.
        var effectiveShelf = EffectiveShelfHeight;
        if (_replacementSession is not null)
        {
            _replacementSession.ShelfHeight = effectiveShelf;
            // The external top gap is part of the dock's headroom: let the window grow with it, exactly like the
            // size/scale setting does, instead of compressing the bar inside a fixed-height window.
            _replacementSession.HeadroomHeight = EffectiveHeadroom;
        }
        else if (IsVertical)
        {
            Width = EffectiveHeadroom + effectiveShelf;
        }
        else
        {
            Height = EffectiveHeadroom + effectiveShelf;
        }
        ApplySecondaryShelf();
    }

    /// <summary>
    /// Grows the fixed divider margins by the configured icon gap so spacing stays visually even between icons
    /// and the separators that group shell buttons, app groups and widgets. Zero preserves the original margins.
    /// </summary>
    private void ApplyDividerPadding(double padding)
    {
        StartDivider.Margin = new Thickness(3 + padding, 0, 4 + padding, 0);
        TrayGroupDivider.Margin = new Thickness(6 + padding, 0, 4 + padding, 0);
        WidgetDivider.Margin = new Thickness(6 + padding, 0, 6 + padding, 0);
    }

    protected override void OnDpiChanged(DpiScale oldDpi, DpiScale newDpi)
    {
        base.OnDpiChanged(oldDpi, newDpi);
        UpdatePreferredIconPixels();
        _runningAppIcons.Clear();
        ThemePacks_Changed(this, EventArgs.Empty);
    }

    private void UpdatePreferredIconPixels()
    {
        var dpiScale = VisualTreeHelper.GetDpi(this).DpiScaleX;
        // Use the native-snapped icon size directly. The forced 128 px extraction backfired: Shell returned
        // upscaled/soft bitmaps that looked blurry once drawn at the small dock size.
        var pixels = ApplicationIconProvider.ComputeApplicationIconPixels(Math.Max(18, Profile.Icon * 0.95), dpiScale);
        ApplicationIconProvider.PreferredIconPixels = pixels;
        ApplicationIconProvider.HoverIconPixels = ApplicationIconProvider.ComputeHoverIconPixels(pixels, DockMetrics.HoverScale);
        ThemePacks.PreferredIconPixels = pixels;
        // Draw the snapped bitmap at its native pixel size, capped to the dock cell so it never clips on the
        // right/bottom (e.g. LibreWolf at medium dock size).
        var appIconSize = DockMetrics.AppIconDipSize(Profile.Icon, pixels, dpiScale);
        Application.Current.Resources["DockApplicationIconSize"] = appIconSize;
        Application.Current.Resources["StartLogoSize"] = appIconSize;
    }

    private void WidgetStrip_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (!e.WidthChanged) return;
        ResizeDockToContent();
        _replacementSession?.RefreshPosition();
    }

    private void ApplyDockAlignment()
    {
        if (IsVertical)
        {
            DockSurface.HorizontalAlignment = Edge == DockEdge.Left ? HorizontalAlignment.Left : HorizontalAlignment.Right;
            DockSurface.VerticalAlignment = IsFullWidth ? VerticalAlignment.Stretch : _dockPosition switch
            {
                DockPosition.Left => VerticalAlignment.Top,
                DockPosition.Right => VerticalAlignment.Bottom,
                _ => VerticalAlignment.Center
            };
            _replacementSession?.RefreshPosition();
            return;
        }

        DockSurface.VerticalAlignment = VerticalAlignment.Bottom;
        if (IsFullWidth)
        {
            DockSurface.HorizontalAlignment = HorizontalAlignment.Stretch;
            _replacementSession?.RefreshPosition();
            return;
        }

        var alignment = _dockPosition switch
        {
            DockPosition.Left => HorizontalAlignment.Left,
            DockPosition.Center => HorizontalAlignment.Center,
            DockPosition.Right => HorizontalAlignment.Right,
            _ => throw new InvalidOperationException("ErrorDockPosition")
        };

        DockSurface.HorizontalAlignment = alignment;
        _replacementSession?.RefreshPosition();
    }

    private void ApplyBackdropHitTesting() => DockBackdrop.Visibility =
        _preferences.Current.AutoHide == AutoHideMode.Disabled ? Visibility.Visible : Visibility.Collapsed;

    private void ApplyTaskbarLayout(Size area)
    {
        _dockArea = area;
        var profile = Profile;
        var thickness = IsVertical ? area.Width : area.Height;
        _dockTopGap = Math.Max(0.0, thickness - profile.SurfaceHeight - profile.BottomGap);
        ApplyDockMargin();
        ResizeDockToContent();
    }

    /// <summary>
    /// Lock/unlock leaves the secure desktop in charge of focus and input for a while. Clear every cached window
    /// handle and transient popup so the first click or keystroke after returning cannot activate a stale window
    /// or land on a flyout action such as a Windows settings link.
    /// </summary>
    internal void ResetTransientUiStateForSessionSwitch(bool unlocking)
    {
        if (unlocking) _widgetCoordinator.SuppressHover(TimeSpan.FromMilliseconds(1200));
        else _widgetCoordinator.CollapseHover();

        _widgetPanel.Close(restoreFocus: false);
        _previewHoverTimer.Stop();
        _previewCloseTimer.Stop();
        _pendingPreviewButton = null;
        _thumbnailPreview.Hide();
        if (_openContextMenu is not null) _openContextMenu.IsOpen = false;
        _openContextMenu = null;
        DockFlyout.CloseAll(restoreFocus: false);
        ResetMagnification();

        _foregroundWindow = 0;
        _foregroundAtMenuOpen = 0;
        _foregroundAtAppPress = null;
        _startMenuOpenAtPress = false;
        _startMenu.Reset();
        _lastApplicationWindow = 0;
        _contextTargetWasForeground = false;
        _contextMenuOpenedAtUtc = default;
        _keyboardNavigationActive = false;
        _foregroundBeforeKeyboard = 0;

        if (!unlocking) return;

        // A Win keydown synthesized just before the lock may have lost its keyup on the secure desktop.
        ShellShortcuts.ReleaseStuckModifiers();
        _windowTracker?.ResyncForeground();
        UnregisterHotkeys();
        RegisterHotkeys();
        _replacementSession?.UpdateForeground(NativeWindowTracker.GetForegroundApplicationWindow());
        _autoHideController?.EvaluateState();
    }

    private void OnFullScreenChanged(bool suppressed)
    {
        if (!suppressed) return;
        _widgetCoordinator.CollapseHover();
        _widgetPanel.Close(restoreFocus: false);
        _previewHoverTimer.Stop();
        _pendingPreviewButton = null;
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        if (_openContextMenu is not null) _openContextMenu.IsOpen = false;
        DockFlyout.CloseAll();
        ResetMagnification();
        UpdateSecondaryDocks();
    }

    private bool _deferredDockResize;
    private double _dockWidthAnimationTarget = double.NaN;

    private void ResizeDockToContent()
    {
        // Defer background content-driven resizes (e.g. widget ticks or width updates) while fisheye
        // magnification is actively expanding button slots; clearing magnification overrides mid-hover
        // snaps icons back to rest for one frame and causes a visible shrink/grow jitter.
        if (_magnificationLayoutActive || _magnificationRendering)
        {
            _deferredDockResize = true;
            return;
        }

        // Measuring runs pending layout, whose SizeChanged handlers call back here. Those calls are coalesced into
        // one more fit by the outer call, which is capped so layout feedback cannot loop.
        if (_resizingDock)
        {
            _resizePending = true;
            return;
        }

        _resizingDock = true;
        try
        {
            var passes = 0;
            do
            {
                _resizePending = false;
                FitDockToContent();
            } while (_resizePending && ++passes < 2);
        }
        finally
        {
            _resizingDock = false;
            _resizePending = false;
        }
    }

    private bool CanExpandWidgetInline(OpenTaskbar.Widgets.DockWidget widget)
    {
        if (!IsVisible || _dockArea.Width <= 0) return false;
        var totalAvailableWidth = Math.Max(1, _dockArea.Width - DockSurface.Margin.Left - DockSurface.Margin.Right);
        var extraWidth = widget.FullWidthEstimate - widget.ActualWidth;
        return DockContents.DesiredSize.Width + Math.Max(0, extraWidth) <= totalAvailableWidth - 2;
    }

    private void FitDockToContent()
    {
        ClearMagnificationLayoutOverrides();
        _magnificationTargetsDirty = true;
        if (IsVertical)
        {
            ResizeSideDockToContent();
            return;
        }

        DockSurface.Height = double.NaN;
        var hasWidgets = _widgetCoordinator.ActiveWidgets.Values.Any(widget => widget.Visibility == Visibility.Visible);
        WidgetIsland.Visibility = hasWidgets ? Visibility.Visible : Visibility.Collapsed;
        var leadWidth = DockScrollViewer.MeasureContentWidth(DockLeadControls);
        var appsContentWidth = IsFullWidth ? 0 : DockScrollViewer.MeasureContentWidth(DockContents);
        var trailWidth = DockScrollViewer.MeasureContentWidth(DockTrailControls);
        var widgetsWidth = hasWidgets ? DockScrollViewer.MeasureContentWidth(WidgetIsland) : 0;
        var targetDisplay = DisplayService.GetTargetDisplay(_preferences.Current.TargetDisplay);
        var availableWidth = _dockArea.Width > 0 ? _dockArea.Width : targetDisplay.Width;
        var margin = DockSurface.Margin;
        var maximumSurfaceWidth = Math.Max(1, availableWidth - margin.Left - margin.Right);
        var maxLayoutWidth = Math.Max(1, maximumSurfaceWidth - DockHorizontalChrome);
        if (!IsFullWidth && _dockWidthOverride is { } fixedWidth)
            maxLayoutWidth = Math.Min(maxLayoutWidth, Math.Max(1, fixedWidth - DockHorizontalChrome));
        // App icons can scroll inside DockScrollViewer, so their natural (unscrolled) width must not be
        // counted as overflow right away. They may take space from their own scroll range first, but once
        // that would squeeze them below a couple of cells the remaining overflow has to come from widgets.
        var maxAppsWidth = Math.Max(0, maxLayoutWidth - leadWidth - trailWidth - widgetsWidth);
        var appScrollFloor = (double)FindResource("DockCellSize") * 2;
        var effectiveAppsWidth = WidgetCoordinator.EffectiveAppsWidth(appsContentWidth, maxAppsWidth, appScrollFloor);
        var contentFixedWidth = leadWidth + effectiveAppsWidth + trailWidth + widgetsWidth;
        // When the user has manually resized the dock, the shrink is intentional for the apps area only.
        // Widget compact mode must not be triggered by the dock width override — only by genuine content overflow
        // relative to the monitor width. Pass the full monitor layout width so Auto widgets never compact
        // just because the user dragged the dock narrower.
        var densityBudget = _dockWidthOverride is not null
            ? Math.Max(1, maximumSurfaceWidth - DockHorizontalChrome)
            : maxLayoutWidth;
        if (_widgetCoordinator.UpdateAdaptiveDensity(densityBudget, contentFixedWidth, _preferences.Current, widgetScale: Profile.WidgetScale))
            _resizePending = true; // Measure again after the widget templates switch to compact mode.

        DockScrollViewer.MaxWidth = maxAppsWidth;
        if (appsContentWidth <= maxAppsWidth + 0.5 && DockScrollViewer.HorizontalOffset > 0)
            DockScrollViewer.ScrollToHorizontalOffset(0);

        if (IsFullWidth)
        {
            DockLayoutRoot.MinWidth = Math.Max(0, maximumSurfaceWidth - DockHorizontalChrome);
            DockSurface.Width = double.NaN;
            if (_dockArea.Width <= 0) Width = Math.Max(16, availableWidth);
            return;
        }

        DockLayoutRoot.MinWidth = 0;
        var desiredWidth = Math.Ceiling(contentFixedWidth + DockHorizontalChrome);
        var targetWidth = Math.Clamp(_dockWidthOverride ?? desiredWidth,
            Math.Min(MinimumDockSurfaceWidth, maximumSurfaceWidth), maximumSurfaceWidth);

        if (!double.IsNaN(_dockWidthAnimationTarget) && Math.Abs(_dockWidthAnimationTarget - targetWidth) < 0.5)
        {
            // Keep the in-flight animation to the same target width running instead of cancelling it
            // (which would briefly snap DockSurface.Width back to its pre-animation base value).
            if (_dockArea.Width <= 0)
                Width = targetWidth + margin.Left + margin.Right;
            return;
        }

        _dockWidthAnimationTarget = double.NaN;
        DockSurface.BeginAnimation(WidthProperty, null);

        if (!double.IsNaN(DockSurface.Width)
            && DockSurface.ActualWidth > 0
            && _dockWidthOverride is null
            && LeftResizeThumb.IsDragging == false && RightResizeThumb.IsDragging == false
            && !_instantDockResize
            && _preferences.Current.EffectiveAnimationsEnabled
            && SystemParameters.ClientAreaAnimation
            && Math.Abs(DockSurface.ActualWidth - targetWidth) >= 1.0)
        {
            var anim = new DoubleAnimation
            {
                From = DockSurface.ActualWidth,
                To = targetWidth,
                Duration = TimeSpan.FromMilliseconds(120),
                EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
            };
            _dockWidthAnimationTarget = targetWidth;
            anim.Completed += (_, _) =>
            {
                if (Math.Abs(_dockWidthAnimationTarget - targetWidth) >= 0.5) return;
                _dockWidthAnimationTarget = double.NaN;
                DockSurface.BeginAnimation(WidthProperty, null);
                DockSurface.Width = targetWidth;
            };
            DockSurface.BeginAnimation(WidthProperty, anim);
        }
        else
        {
            DockSurface.Width = targetWidth;
        }
        if (_dockArea.Width <= 0)
            Width = targetWidth + margin.Left + margin.Right;
    }

    internal static bool ExcludeFromTaskSwitcher(nint windowHandle)
    {
        Marshal.SetLastPInvokeError(0);
        var currentStyle = GetWindowLongPtr(windowHandle, GwlExStyle);
        if (currentStyle == 0 && Marshal.GetLastPInvokeError() != 0)
        {
            return false;
        }

        var style = new nint((currentStyle.ToInt64() | WsExToolWindow) & ~WsExAppWindow);
        if (style != currentStyle)
        {
            Marshal.SetLastPInvokeError(0);
            var previousStyle = SetWindowLongPtr(windowHandle, GwlExStyle, style);
            if (previousStyle == 0 && Marshal.GetLastPInvokeError() != 0)
            {
                return false;
            }
        }

        return SetWindowPos(windowHandle, 0, 0, 0, 0, 0,
            SwpNoSize | SwpNoMove | SwpNoZOrder | SwpNoActivate | SwpFrameChanged);
    }

    private void ExitOpenTaskbar_Click(object sender, RoutedEventArgs e)
    {
        Close();
    }

    internal string? RestoreWindowsTaskbar()
    {
        try
        {
            // Hand tray icons back to Explorer before its taskbar becomes visible again.
            TrayHost.Current?.Dispose();
            var session = _replacementSession;
            if (session is null) return null;
            var error = session.Stop();
            if (session.IsActive) return error;
            session.OperationReported -= ReportOperationStatus;
            session.LayoutChanged -= ApplyTaskbarLayout;
            session.FullScreenChanged -= OnFullScreenChanged;
            session.Dispose();
            _replacementSession = null;
            return error;
        }
        finally
        {
            // Start, Search and Control Center are long-lived: move them back before this process goes away.
            ShellShortcuts.RestoreShellFlyoutPositions();
        }
    }

    private void Window_Closing(object? sender, CancelEventArgs e)
    {
        var error = RestoreWindowsTaskbar();
        var restoreUncertain = error is not null || TaskbarReplacementSession.HasPendingRecovery
            || _replacementSession?.IsActive == true;
        if (restoreUncertain)
        {
            // Stop() can fail while Explorer is restarting and still clear its active flags. The journal is the
            // authoritative signal, so always try the journal-based restore before allowing the window to close.
            var forced = TaskbarReplacementSession.ForceRestoreNativeTaskbar();
            if (forced is null && _replacementSession?.IsActive != true) error = null;
            else error ??= forced;
        }

        if (error is not null || TaskbarReplacementSession.HasPendingRecovery || _replacementSession?.IsActive == true)
        {
            ReportError(error ?? "ErrorTaskbarRestoreFailed");
        }
    }

    private void Window_Closed(object? sender, EventArgs e)
    {
        _widgetPanel.IsOpenChanged -= WidgetPanel_IsOpenChanged;
        _widgetPanel.OperationFailed -= ReportError;
        _widgetPanel.Dispose();
        ShellShortcuts.IsMediaWidgetActive = null;
        _preferences.Changed -= Preferences_Changed;
        _settingsWindow?.CloseForShutdown();
        _jumpListLoad?.Cancel();
        _jumpListLoad?.Dispose();
        if (_openContextMenu is not null) _openContextMenu.IsOpen = false;
        _openContextMenu = null;
        ResetMagnification(immediate: true);
        _restingIcons.Clear();
        _source?.RemoveHook(WindowProc);
        _source = null;

        if (_windowTracker is not null)
        {
            _windowTracker.WindowsChanged -= WindowTracker_WindowsChanged;
            _windowTracker.ForegroundWindowChanged -= WindowTracker_ForegroundWindowChanged;
            _windowTracker.Dispose();
            _windowTracker = null;
        }

        _previewCloseTimer.Stop();
        _previewCloseTimer.Tick -= PreviewCloseTimer_Tick;
        _previewHoverTimer.Stop();
        _previewHoverTimer.Tick -= PreviewHoverTimer_Tick;
        _widgetRefreshTimer.Stop();
        _widgetRefreshTimer.Tick -= WidgetRefreshTimer_Tick;
        _iconRetryTimer.Stop();
        _iconRetryTimer.Tick -= IconRetryTimer_Tick;
        _timedWidgetRefreshTimer.Stop();
        _timedWidgetRefreshTimer.Tick -= TimedWidgetRefreshTimer_Tick;
        _foregroundPollTimer.Stop();
        _foregroundPollTimer.Tick -= ForegroundPollTimer_Tick;
        foreach (var widget in _widgets.OfType<ITimedDockWidget>()) widget.PropertyChanged -= TimedWidget_PropertyChanged;
        _thumbnailPreview.PointerEntered -= ThumbnailPreview_PointerEntered;
        _thumbnailPreview.PointerLeft -= ThumbnailPreview_PointerLeft;
        _thumbnailPreview.PreviewClicked -= ThumbnailPreview_PreviewClicked;
        _thumbnailPreview.CloseRequested -= ThumbnailPreview_CloseRequested;
        _thumbnailPreview.Dispose();
        _autoHideController?.Dispose();
        _autoHideController = null;
        _startMenu.Dispose();
        _widgetCoordinator.Dispose();
        _jumpLists.Dispose();
        _wheelAudio?.Dispose();
        _wheelAudio = null;
        DisposeExtras();
        DisposeFeatures();
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindow(nint windowHandle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsIconic(nint windowHandle);

    [DllImport("user32.dll")]
    private static extern void keybd_event(byte virtualKey, byte scanCode, uint flags, nuint extraInfo);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW", SetLastError = true)]
    internal static extern nint GetWindowLongPtr(nint windowHandle, int index);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessage(nint windowHandle, uint message, nint wParam, nint lParam);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint windowHandle, out uint processId);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetForegroundWindow(nint windowHandle);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW", SetLastError = true)]
    private static extern nint SetWindowLongPtr(nint windowHandle, int index, nint value);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint windowHandle, nint insertAfter, int x, int y, int width, int height, uint flags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "RegisterWindowMessageW")]
    private static extern uint RegisterWindowMessage(string message);

    private void ScrollPrevOverlay_Click(object sender, RoutedEventArgs e)
        => DockScrollViewer.ScrollStepBack();

    private void ScrollNextOverlay_Click(object sender, RoutedEventArgs e)
        => DockScrollViewer.ScrollStepForward();
}
