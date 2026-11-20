using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Input;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>Global hotkeys, keyboard navigation, profiles, update checks, and the virtual desktop filter.</summary>
public partial class MainWindow
{
    private const int WmHotkey = 0x0312;
    private const int DockHotkeyId = 0x4F02;
    private HotkeyChoice? _registeredDockHotkey;
    private bool _keyboardNavigationActive;
    private nint _foregroundBeforeKeyboard;
    private UpdateChecker.Release? _availableUpdate;
    private DispatcherTimer? _updateTimer;
    private CancellationTokenSource? _updateCheck;
    private DispatcherTimer? _desktopTimer;
    private int _currentDesktop = 1;
    private IReadOnlyList<TrackedWindow>? _filteredSource;
    private IReadOnlyList<TrackedWindow> _filteredWindows = [];
    private int _filteredDesktop;
    private string? _appliedProfileSignature;
    private bool _profileFilterApplied;
    private readonly List<SecondaryDockWindow> _secondaryDocks = [];
    private DispatcherTimer? _secondaryTimer;

    private void OnAnyDockFlyoutClosed() => _autoHideController?.EvaluateState();

    private void InitializeFeatures()
    {
        Deactivated += (_, _) =>
        {
            EndKeyboardNavigation(restoreForeground: false);
            DockFlyout.CloseAll();
            _autoHideController?.EvaluateState();
        };
        DockFlyout.AnyClosed += OnAnyDockFlyoutClosed;
        PreviewKeyDown += Dock_PreviewKeyDown;
        _updateTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher) { Interval = TimeSpan.FromSeconds(12) };
        _updateTimer.Tick += async (_, _) =>
        {
            _updateTimer!.Interval = TimeSpan.FromHours(24);
            await CheckForUpdatesAsync(userInitiated: false);
        };
        _desktopTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher) { Interval = TimeSpan.FromSeconds(1) };
        _desktopTimer.Tick += (_, _) => RefreshDesktopFilter();
        ApplyFeatureSettings();
    }

    private void ApplyFeatureSettings()
    {
        if (_updateTimer is null) return;
        RegisterHotkeys();
        if (_preferences.Current.CheckUpdates) { if (!_updateTimer.IsEnabled) _updateTimer.Start(); }
        else _updateTimer.Stop();
        if (_preferences.Current.CurrentDesktopOnly) _desktopTimer?.Start();
        else _desktopTimer?.Stop();
        _filteredSource = null;
        ApplyStartPlacement();
        ApplySearchButton();
        ApplyShellButtons();
        ApplyEdge();
        ApplySecondaryDocks();
        ApplyProfileFilter();
    }

    private bool StartButtonShown => _preferences.Current.StartPlacement != StartButtonPlacement.Hidden;

    /// <summary>Only controls visibility; the unified dock order decides where the Start button sits.</summary>
    private void ApplyStartPlacement()
    {
        StartButton.Visibility = _preferences.Current.StartPlacement == StartButtonPlacement.Hidden
            ? Visibility.Collapsed
            : Visibility.Visible;
        // The divider belonged to the old grouped layout and is no longer rendered.
        StartDivider.Visibility = Visibility.Collapsed;
    }

    /// <summary>Only controls visibility; the unified dock order decides where the Search button sits.</summary>
    private void ApplySearchButton()
    {
        SearchButton.Visibility = _preferences.Current.ShowSearchButton
            ? Visibility.Visible
            : Visibility.Collapsed;
    }

    /// <summary>Visibility for the permanent shell buttons; their position comes from the unified dock order.</summary>
    private void ApplyShellButtons()
    {
        TaskViewButton.Visibility = _preferences.Current.ShowTaskViewButton
            ? Visibility.Visible : Visibility.Collapsed;
        TrayButton.Visibility = _preferences.Current.ShowTrayButton
            ? Visibility.Visible : Visibility.Collapsed;
        ShowDesktopButton.Visibility = _preferences.Current.ShowDesktopButton
            ? Visibility.Visible : Visibility.Collapsed;
    }

    private void TaskViewButton_Click(object sender, RoutedEventArgs e) =>
        ReportResult(VirtualDesktops.TaskView());

    private void PreviousDesktop_Click(object sender, RoutedEventArgs e) =>
        ReportResult(VirtualDesktops.Previous());

    private void NextDesktop_Click(object sender, RoutedEventArgs e) =>
        ReportResult(VirtualDesktops.Next());

    private void NewDesktop_Click(object sender, RoutedEventArgs e) =>
        ReportResult(VirtualDesktops.Create());

    private void ShowDesktop_Click(object sender, RoutedEventArgs e) =>
        ReportResult(ShellShortcuts.ToggleShowDesktop());

    // ---- Global hotkeys (#2) ----

    internal static (uint Modifiers, uint Key)? DescribeHotkey(HotkeyChoice choice) => choice switch
    {
        HotkeyChoice.AltSpace => (0x1, 0x20),
        HotkeyChoice.CtrlSpace => (0x2, 0x20),
        HotkeyChoice.CtrlAltSpace => (0x3, 0x20),
        HotkeyChoice.CtrlAltD => (0x3, 0x44),
        // No MOD_WIN hotkey: a Win keydown lost during a lock/secure-desktop transition can make a later
        // ordinary keystroke trigger this registration (and other Win shortcuts) with no physical Win press.
        _ => null
    };

    private void RegisterHotkeys()
    {
        if (_source is null) return;
        var wanted = _preferences.Current.DockHotkey;
        if (_registeredDockHotkey == wanted) return;
        UnregisterHotkeys();
        _registeredDockHotkey = wanted;
        if (DescribeHotkey(wanted) is not { } hotkey) return;
        const uint noRepeat = 0x4000;
        if (!RegisterHotKey(_source.Handle, DockHotkeyId, hotkey.Modifiers | noRepeat, hotkey.Key))
        {
            AppLog.Warning($"RegisterHotKey {wanted} failed: {Marshal.GetLastPInvokeError()}");
            ReportError("ErrorHotkeyRegister");
        }
    }

    private void UnregisterHotkeys()
    {
        if (_source is null) return;
        UnregisterHotKey(_source.Handle, DockHotkeyId);
        _registeredDockHotkey = null;
    }

    private bool HandleHotkeyMessage(int message, nint wParam)
    {
        if (message != WmHotkey) return false;
        switch (wParam.ToInt32())
        {
            case DockHotkeyId:
                if (_keyboardNavigationActive) EndKeyboardNavigation(restoreForeground: true);
                else StartKeyboardNavigation();
                return true;
            default:
                return false;
        }
    }

    private void SearchButton_Click(object sender, RoutedEventArgs e) =>
        ReportResult(ShellShortcuts.OpenWindowsSearch(SearchButton));

    private void OpenSearch_Click(object sender, RoutedEventArgs e) => ReportResult(ShellShortcuts.OpenWindowsSearch());

    private void OpenTaskManager_Click(object sender, RoutedEventArgs e) => ReportResult(ShellShortcuts.OpenTaskManager());

    // ---- Keyboard navigation and accessibility (#16) ----

    private void StartKeyboardNavigation()
    {
        _foregroundBeforeKeyboard = NativeWindowTracker.GetForegroundApplicationWindow();
        _keyboardNavigationActive = true;
        _autoHideController?.AnimateReveal();
        _thumbnailPreview.Hide();
        // WM_HOTKEY grants this process foreground rights, so the dock can take keyboard focus for a moment.
        Activate();
        var first = GetAllDockButtons().FirstOrDefault(button => button.IsVisible);
        first?.Focus();
        Keyboard.Focus(first);
    }

    private void EndKeyboardNavigation(bool restoreForeground)
    {
        if (!_keyboardNavigationActive) return;
        _keyboardNavigationActive = false;
        Keyboard.ClearFocus();
        if (restoreForeground && _foregroundBeforeKeyboard != 0) NativeWindowTracker.Activate(_foregroundBeforeKeyboard);
        _autoHideController?.EvaluateState();
    }

    private void Dock_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (!_keyboardNavigationActive) return;
        var vertical = _preferences.Current.Edge != DockEdge.Bottom;
        var key = e.Key == Key.System ? e.SystemKey : e.Key;
        var forward = vertical ? Key.Down : Key.Right;
        var backward = vertical ? Key.Up : Key.Left;
        if (key == Key.Escape)
        {
            e.Handled = true;
            EndKeyboardNavigation(restoreForeground: true);
            return;
        }

        if (key != forward && key != backward && key != Key.Home && key != Key.End) return;
        // Buttons live in several Grid columns (lead strip, scroller, trail strip), so translate against the shared
        // surface rather than DockContents: absolute coordinates keep navigation in visual order in every edge mode.
        var buttons = GetAllDockButtons().Where(button => button.IsVisible && button.ActualWidth > 0)
            .OrderBy(button => vertical ? button.TranslatePoint(default, DockSurface).Y : button.TranslatePoint(default, DockSurface).X)
            .ToList();
        if (buttons.Count == 0) return;
        var current = buttons.FindIndex(button => button.IsKeyboardFocusWithin);
        var target = key switch
        {
            Key.Home => 0,
            Key.End => buttons.Count - 1,
            _ when key == forward => Math.Min(buttons.Count - 1, current + 1),
            _ => Math.Max(0, current - 1)
        };
        buttons[target].Focus();
        e.Handled = true;
    }

    // ---- Profiles (#14) ----

    private DockProfile? ActiveProfile => _preferences.Current.Profiles?.FirstOrDefault(profile =>
        string.Equals(profile.Name, _preferences.Current.ActiveProfile, StringComparison.CurrentCultureIgnoreCase));

    private bool IsHiddenByProfile(DockApp app) =>
        ActiveProfile?.HiddenApps.Contains(app.ExecutablePath, StringComparer.OrdinalIgnoreCase) == true;

    private int VisiblePinnedCount => _pinnedApps.Count(app => !IsHiddenByProfile(app));

    private void ApplyProfileFilter()
    {
        // Reassigning the filter rebuilds every pinned button, so only do it when the hidden set actually changes.
        var signature = ActiveProfile is { } profile
            ? string.Join("|", profile.HiddenApps.Order(StringComparer.OrdinalIgnoreCase))
            : null;
        if (_appliedProfileSignature == signature && _profileFilterApplied) return;
        _appliedProfileSignature = signature;
        _profileFilterApplied = true;
        var view = CollectionViewSource.GetDefaultView(_pinnedApps);
        view.Filter = signature is null ? null : item => item is DockApp app && !IsHiddenByProfile(app);
        if (_windowTracker is not null) RefreshRunningApps();
        ResizeDockToContent();
        _replacementSession?.RefreshPosition();
    }

    private void SaveProfiles(IReadOnlyList<DockProfile>? profiles, string? active)
    {
        var error = _preferences.Save(_preferences.Current with { Profiles = profiles is { Count: > 0 } ? profiles : null, ActiveProfile = active });
        if (error is not null) ReportError(error);
    }

    private void PopulateProfilesMenu(MenuItem menu)
    {
        menu.Items.Clear();
        var profiles = _preferences.Current.Profiles ?? [];
        var active = ActiveProfile;
        var all = new MenuItem { Header = UiText.Get("ProfileAll"), IsCheckable = true, IsChecked = active is null };
        all.Click += (_, _) => SaveProfiles(profiles, null);
        menu.Items.Add(all);
        foreach (var profile in profiles)
        {
            var item = new MenuItem { Header = profile.Name.Replace("_", "__"), IsCheckable = true, IsChecked = ReferenceEquals(profile, active) };
            item.Click += (_, _) => SaveProfiles(profiles, profile.Name);
            menu.Items.Add(item);
        }

        menu.Items.Add(new Separator());
        var create = new MenuItem { Header = UiText.Get("ProfileNew"), Icon = FindResource("IconPlus") };
        create.Click += (_, _) => CreateProfile();
        menu.Items.Add(create);
        if (active is null) return;
        if (active.HiddenApps.Count > 0)
        {
            var reveal = new MenuItem { Header = UiText.Get("ProfileShowAll") };
            reveal.Click += (_, _) => SaveProfiles(
                profiles.Select(profile => ReferenceEquals(profile, active) ? profile with { HiddenApps = [] } : profile).ToList(), active.Name);
            menu.Items.Add(reveal);
        }

        var delete = new MenuItem { Header = UiText.Format("ProfileDelete", active.Name.Replace("_", "__")), Icon = FindResource("IconClose") };
        delete.SetResourceReference(StyleProperty, "DestructiveMenuItem");
        delete.Click += (_, _) => SaveProfiles(profiles.Where(profile => !ReferenceEquals(profile, active)).ToList(), null);
        menu.Items.Add(delete);
    }

    private void CreateProfile()
    {
        var name = TextPrompt.Ask(UiText.Get("ProfileNew"), UiText.Get("ProfileNamePrompt"));
        if (string.IsNullOrWhiteSpace(name)) return;
        name = name.Trim();
        var profiles = (_preferences.Current.Profiles ?? []).ToList();
        if (profiles.Any(profile => string.Equals(profile.Name, name, StringComparison.CurrentCultureIgnoreCase)))
        {
            ReportError("ErrorProfileExists");
            return;
        }

        profiles.Add(new DockProfile(name, []));
        SaveProfiles(profiles, name);
    }

    private void HideInProfile_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuItem { CommandParameter: DockApp app } || ActiveProfile is not { } active) return;
        var profiles = (_preferences.Current.Profiles ?? []).Select(profile => ReferenceEquals(profile, active)
            ? profile with { HiddenApps = profile.HiddenApps.Append(app.ExecutablePath).Distinct(StringComparer.OrdinalIgnoreCase).ToList() }
            : profile).ToList();
        SaveProfiles(profiles, active.Name);
    }

    /// <summary>Pinning an app the active profile hides only shows it again; the Windows pin already exists.</summary>
    private bool RevealHiddenPin(DockApp app)
    {
        if (ActiveProfile is not { } active || !_pinnedApps.Any(pinned =>
                string.Equals(pinned.ExecutablePath, app.ExecutablePath, StringComparison.OrdinalIgnoreCase)))
        {
            return false;
        }

        var profiles = (_preferences.Current.Profiles ?? []).Select(profile => ReferenceEquals(profile, active)
            ? profile with { HiddenApps = profile.HiddenApps.Where(path => !string.Equals(path, app.ExecutablePath, StringComparison.OrdinalIgnoreCase)).ToList() }
            : profile).ToList();
        SaveProfiles(profiles, active.Name);
        return true;
    }

    // ---- Extra launch commands in the app menu (#3) ----

    private void NewInstance_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuItem { CommandParameter: DockApp app }) LaunchApp(app, elevated: false);
    }

    private void RunAsAdmin_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuItem { CommandParameter: DockApp app }) LaunchApp(app, elevated: true);
    }

    // ---- Update checks (#15) ----

    private bool _isUpdating;

    private async Task CheckForUpdatesAsync(bool userInitiated)
    {
        _updateCheck?.Cancel();
        var check = new CancellationTokenSource();
        _updateCheck = check;
        var (release, succeeded) = await UpdateChecker.FindNewerAsync(check.Token);
        if (!ReferenceEquals(_updateCheck, check) || !IsLoaded) return;
        if (!succeeded)
        {
            // Background checks stay quiet when offline; a manual check still records the failure.
            if (userInitiated) AppLog.ReportError("ErrorUpdateCheck");
            return;
        }

        _availableUpdate = release;
        if (release is not null)
        {
            NotificationService.ShowUpdateToast(release.Tag, release.Url);
        }
        else if (userInitiated)
        {
            NotificationService.ShowInfoToast(UiText.Format("UpdateCurrent", UpdateChecker.CurrentVersion.ToString(3)));
        }
    }

    private async Task StartUpdateAsync(UpdateChecker.Release release)
    {
        if (_isUpdating)
        {
            NotificationService.ShowInfoToast(UiText.Get("UpdateInProgress"));
            return;
        }

        if (string.IsNullOrEmpty(release.DownloadUrl))
        {
            ReportResult(ShellShortcuts.OpenUri(release.Url));
            return;
        }

        _isUpdating = true;
        NotificationService.ShowInfoToast(UiText.Format("UpdateDownloading", release.Tag));

        try
        {
            var (success, errorKey, httpStatusCode) = await Task.Run(() => UpdateChecker.DownloadAndApplyAsync(release));
            if (!success)
            {
                _isUpdating = false;
                if (errorKey is { } key)
                    AppLog.ReportError(key, httpStatusCode is { } statusCode
                        ? new object[] { statusCode }
                        : []);
                else
                    AppLog.ReportError("UpdateFailed");
            }
        }
        catch (Exception ex)
        {
            _isUpdating = false;
            AppLog.ReportError("UpdateFailed", ex);
        }
    }

    private async void CheckUpdates_Click(object sender, RoutedEventArgs e)
    {
        if (_availableUpdate is { } release)
        {
            await StartUpdateAsync(release);
            return;
        }

        await CheckForUpdatesAsync(userInitiated: true);
    }

    // ---- Virtual desktop filter (#7) ----

    /// <summary>The tracked windows, limited to the current virtual desktop when that option is on.</summary>
    private IReadOnlyList<TrackedWindow> VisibleSnapshot => FilterWindows(_windowTracker?.Snapshot ?? []);

    private IReadOnlyList<TrackedWindow> FilterWindows(IReadOnlyList<TrackedWindow> windows)
    {
        if (!_preferences.Current.CurrentDesktopOnly) return windows;
        if (ReferenceEquals(windows, _filteredSource) && _filteredDesktop == _currentDesktop) return _filteredWindows;
        _filteredSource = windows;
        _filteredDesktop = _currentDesktop;
        _filteredWindows = windows.Where(window => VirtualDesktops.IsOnCurrentDesktop(window.Handle)).ToList();
        return _filteredWindows;
    }

    private void RefreshDesktopFilter()
    {
        var (current, _) = VirtualDesktops.Read();
        if (current == _currentDesktop || _windowTracker is null) return;
        _currentDesktop = current;
        WindowTracker_WindowsChanged(this, _windowTracker.Snapshot);
    }

    // ---- Docks on other displays (#10) ----

    private void ApplySecondaryDocks()
    {
        var wanted = _preferences.Current.ShowOnAllMonitors ? SecondaryDockWindow.SecondaryMonitors(_preferences.Current.TargetDisplay) : [];
        foreach (var dock in _secondaryDocks.Where(dock => !wanted.Contains(dock.Monitor)).ToList())
        {
            dock.Close();
            _secondaryDocks.Remove(dock);
        }

        foreach (var monitor in wanted.Where(monitor => _secondaryDocks.All(dock => dock.Monitor != monitor)))
        {
            var dock = new SecondaryDockWindow(monitor, _runningAppIcons, _preferences.Current.SecondaryApps, _preferences.Current.SecondaryWidgets);
            dock.Show();
            _secondaryDocks.Add(dock);
        }

        foreach (var dock in _secondaryDocks)
        {
            dock.Configure(_preferences.Current.SecondaryApps, _preferences.Current.SecondaryWidgets);
            dock.ApplyShelfHeight(Profile.Shelf);
            dock.PositionOnMonitor();
        }
        if (_secondaryDocks.Count > 0)
        {
            _secondaryTimer ??= CreateSecondaryTimer();
            _secondaryTimer.Start();
        }
        else
        {
            _secondaryTimer?.Stop();
        }

        UpdateSecondaryDocks();
    }

    /// <summary>Propagates a size/scale change to the secondary docks' reserved shelves.</summary>
    private void ApplySecondaryShelf()
    {
        if (_secondaryDocks.Count == 0) return;
        var shelf = Profile.Shelf;
        foreach (var dock in _secondaryDocks)
        {
            if (Math.Abs(dock.ShelfHeight - shelf) < 0.01) continue;
            dock.ApplyShelfHeight(shelf);
            dock.PositionOnMonitor();
        }
    }

    private DispatcherTimer CreateSecondaryTimer()
    {
        // Keeps the secondary clocks current and re-measures the shelf Explorer may release after startup.
        var timer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher) { Interval = TimeSpan.FromSeconds(10) };
        timer.Tick += (_, _) =>
        {
            foreach (var dock in _secondaryDocks) dock.PositionOnMonitor();
            UpdateSecondaryDocks();
        };
        return timer;
    }

    private void UpdateSecondaryDocks()
    {
        if (_secondaryDocks.Count == 0) return;
        var candidates = RunningCandidates();
        foreach (var dock in _secondaryDocks) dock.Update(candidates, _foregroundWindow);
    }

    private void DisposeFeatures()
    {
        _secondaryTimer?.Stop();
        foreach (var dock in _secondaryDocks) dock.Close();
        _secondaryDocks.Clear();
        UnregisterHotkeys();
        _updateTimer?.Stop();
        _desktopTimer?.Stop();
        _updateCheck?.Cancel();
        DockFlyout.AnyClosed -= OnAnyDockFlyoutClosed;
    }

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool RegisterHotKey(nint window, int id, uint modifiers, uint key);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnregisterHotKey(nint window, int id);
}
