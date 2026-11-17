namespace OpenTaskbar.Models;

public enum DockPosition
{
    Left,
    Center,
    Right
}

public enum AppTheme { Dark, Light }
public enum AppLanguage { Turkish, English, Japanese }
public enum AutoHideMode { Disabled, Smart, Always }
public enum DockLayout { Floating, FullWidth }
public enum DockSize { Compact, Medium, Large }
public enum DockEdge { Bottom, Left, Right }
public enum HotkeyChoice { Disabled, AltSpace, CtrlSpace, CtrlAltSpace, CtrlAltD, WinAltD }
public enum StartButtonAction { WindowsStart, Launcher }
public enum StartButtonPlacement { Start, End, Hidden }
public enum ClockDateStyle { DayAndShortDate, ShortDateOnly, NumericDate, Hidden }

/// <summary>The individual meters of the System meters widget; a cleared flag hides that meter.</summary>
[Flags]
public enum SystemMeters
{
    None = 0,
    Cpu = 1,
    Memory = 2,
    Battery = 4,
    CpuTemperature = 8,
    Gpu = 16,
    GpuTemperature = 32,
    All = Cpu | Memory | Battery | CpuTemperature | Gpu | GpuTemperature
}

/// <summary>A named view of the dock that hides some pinned apps; pins themselves are never removed.</summary>
public sealed record DockProfile(string Name, IReadOnlyList<string> HiddenApps);

// Missing JSON properties fall back to these constructor defaults, so older settings files stay valid.
public sealed record DockSettings(
    DockPosition Position = DockPosition.Center,
    IReadOnlyList<string>? PinOrder = null,
    AppTheme Theme = AppTheme.Dark,
    AppLanguage Language = AppLanguage.English,
    bool AnimationsEnabled = true,
    bool PreviewsEnabled = true,
    bool NotificationBadgesEnabled = true,
    AutoHideMode AutoHide = AutoHideMode.Disabled,
    double SurfaceOpacity = 0.9,
    IReadOnlyList<string>? Widgets = null,
    IReadOnlyList<string>? InstalledWidgets = null,
    DockLayout Layout = DockLayout.Floating,
    DockSize Size = DockSize.Compact,
    bool MagnificationEnabled = true,
    bool ShowClock = false,
    bool ShowMedia = false,
    bool ShowSystemStats = false,
    bool ShowFocus = false,
    bool ShowAudio = false,
    bool ShowQuickSettings = false,
    int FocusMinutes = 25,
    int FocusBreakMinutes = 5,
    int FocusSessions = 4,
    bool ShowRecycleBin = true,
    HotkeyChoice LauncherHotkey = HotkeyChoice.AltSpace,
    HotkeyChoice DockHotkey = HotkeyChoice.CtrlAltD,
    StartButtonAction StartAction = StartButtonAction.WindowsStart,
    StartButtonPlacement StartPlacement = StartButtonPlacement.Start,
    bool WheelVolume = true,
    bool CheckUpdates = true,
    bool ShowWeather = false,
    string WeatherCity = "",
    bool ShowVirtualDesktops = false,
    bool CurrentDesktopOnly = false,
    bool ShowTray = false,
    bool ShowOnAllMonitors = true,
    DockEdge Edge = DockEdge.Bottom,
    // Changed from the dock itself, not the Settings edit buffer. Null stacks means "Downloads only".
    IReadOnlyList<string>? StackFolders = null,
    IReadOnlyList<DockProfile>? Profiles = null,
    string? ActiveProfile = null,
    double DockScale = 1.0,
    IReadOnlyList<string>? WidgetOrder = null,
    bool Clock24Hour = true,
    bool ClockShowSeconds = false,
    ClockDateStyle DateStyle = ClockDateStyle.DayAndShortDate,
    SystemMeters StatsMeters = SystemMeters.All,
    // Folder name under %LOCALAPPDATA%\OpenTaskbar\Themes; null keeps the built-in look.
    string? ThemePack = null);
