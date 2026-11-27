using System.Globalization;
using System.Runtime.InteropServices;
using OpenTaskbar.Models;
using OpenTaskbar.Widgets;

namespace OpenTaskbar.Services;

/// <summary>
/// The single source of truth for every widget: catalog card, dock slot, label/icon keys and capabilities.
/// Adding one descriptor here makes the widget appear in Settings → Widgets, validates it in settings.json,
/// gives it a deterministic dock slot and — when it has a factory — a live preview. No other list needs editing.
/// </summary>
public sealed record DockWidgetDescriptor(
    string Id,
    string LabelKey,
    Func<CultureInfo, string>? Read,
    DockWidgetCategory Category,
    string IconKey,
    bool HasSettings,
    bool CatalogVisible = true,
    bool SupportsLivePreview = true,
    int? DockOrder = null,
    Func<OpenTaskbar.Widgets.DockWidget>? Factory = null,
    IReadOnlyList<string>? AliasIds = null);

public sealed class DockWidgetService(TimeProvider timeProvider)
{
    private readonly StopwatchDockWidget _stopwatch = new(timeProvider);
    private readonly CountdownDockWidget _countdown = new(timeProvider);
    private const byte UnknownBatteryLifePercent = 255;
    private const byte NoSystemBattery = 128;
    private const byte ChargingBatteryFlag = 8;

    public const string ClockId = "Clock";
    public const string DateId = "Date";
    public const string MediaId = "Media";
    public const string SystemStatsId = "SystemStats";
    public const string FocusId = "Focus";
    public const string AudioId = "Audio";
    public const string QuickSettingsId = "QuickSettings";
    public const string WeatherId = "Weather";
    public const string BatteryId = "Battery";
    public const string StopwatchId = "Stopwatch";
    public const string CountdownId = "Countdown";
    public const string NetSpeedId = "NetSpeed";
    public const string BluetoothBatteryId = "BluetoothBattery";
    public const string KeyboardLayoutId = "KeyboardLayout";

    public sealed record AiUsageWidgetDefinition(string Id, AiProviders Provider, string LabelKey, string IconKey);

    /// <summary>Every AI provider is its own widget so each can be added, ordered and removed independently.</summary>
    public static IReadOnlyList<AiUsageWidgetDefinition> AiUsageWidgets { get; } =
    [
        new("AiUsageClaude", AiProviders.Claude, "WidgetAiUsageClaude", "LogoClaude"),
        new("AiUsageCodex", AiProviders.Codex, "WidgetAiUsageCodex", "LogoCodex"),
        new("AiUsageOpenCode", AiProviders.OpenCode, "WidgetAiUsageOpenCode", "LogoOpenCode"),
        new("AiUsageCopilot", AiProviders.Copilot, "WidgetAiUsageCopilot", "LogoCopilot"),
        new("AiUsageDeepSeek", AiProviders.DeepSeek, "WidgetAiUsageDeepSeek", "LogoDeepSeek"),
        new("AiUsageOpenRouter", AiProviders.OpenRouter, "WidgetAiUsageOpenRouter", "LogoOpenRouter"),
        new("AiUsageCursor", AiProviders.Cursor, "WidgetAiUsageCursor", "LogoCursor")
    ];

    /// <summary>
    /// Declaration order is the catalog order (grouped by Category, stable within a category). DockOrder is the
    /// separate, explicitly numbered dock-strip order — keeping it makes existing WidgetOrder/DockItemOrder
    /// arrays in saved settings continue to mean the same thing after a new widget is added.
    /// </summary>
    private static readonly DockWidgetDescriptor[] Descriptors =
    [
        // Time & Weather
        new(ClockId, "WidgetClock", culture => DateTime.Now.ToString("HH:mm", culture),
            DockWidgetCategory.TimeAndWeather, "IconClock", true,
            DockOrder: 18, Factory: () => new ClockWidget(), AliasIds: [DateId]),
        new(WeatherId, "WidgetWeather", _ => "21°",
            DockWidgetCategory.TimeAndWeather, "IconCloudSun", true,
            DockOrder: 3, Factory: () => new WeatherWidget()),
        new("DayProgress", "WidgetDayProgress", culture =>
                (DateTime.Now.TimeOfDay.TotalSeconds / TimeSpan.FromDays(1).TotalSeconds).ToString("P0", culture),
            DockWidgetCategory.TimeAndWeather, "IconCalendar", false,
            DockOrder: 8, Factory: () => new DayProgressWidget()),
        new(StopwatchId, "WidgetStopwatch", null,
            DockWidgetCategory.TimeAndWeather, "IconTimer", false, SupportsLivePreview: false),
        new(CountdownId, "WidgetCountdown", null,
            DockWidgetCategory.TimeAndWeather, "IconHourglass", false, SupportsLivePreview: false),

        // System & Hardware
        new(SystemStatsId, "WidgetSystemStats", _ => "CPU · RAM",
            DockWidgetCategory.SystemAndHardware, "IconCpu", true,
            DockOrder: 1, Factory: () => new SystemStatsWidget()),
        new(NetSpeedId, "WidgetNetSpeed", _ => "↓ 0 B/s · ↑ 0 B/s",
            DockWidgetCategory.SystemAndHardware, "IconWifi", false,
            DockOrder: 4, Factory: () => new NetSpeedWidget()),
        new(BatteryId, "WidgetBattery", _ => ReadBatteryStatus(),
            DockWidgetCategory.SystemAndHardware, "IconBattery", false,
            DockOrder: 6, Factory: () => new BatteryWidget()),
        new(BluetoothBatteryId, "WidgetBluetoothBattery", _ => "53% · 🔋",
            DockWidgetCategory.SystemAndHardware, "IconBluetooth", false,
            DockOrder: 5, Factory: () => new BluetoothBatteryWidget()),
        new("Storage", "WidgetStorage", SystemWidgetData.ReadStorage,
            DockWidgetCategory.SystemAndHardware, "IconHardDrive", false,
            DockOrder: 7, Factory: () => new StorageWidget()),

        // AI Quotas
        .. AiUsageWidgets.Select((widget, index) => new DockWidgetDescriptor(
            widget.Id, widget.LabelKey, null, DockWidgetCategory.AiQuotas, widget.IconKey, false,
            DockOrder: 9 + index, Factory: () => new AiUsageWidget(widget.Provider))),

        // Media & Controls
        new(MediaId, "WidgetMedia", _ => "▶ · 02:15",
            DockWidgetCategory.MediaAndControls, "IconMusic", true,
            DockOrder: 0, Factory: () => new MediaWidget()),
        new(AudioId, "WidgetAudio", _ => "100%",
            DockWidgetCategory.MediaAndControls, "IconVolume", true,
            DockOrder: 16, Factory: () => new AudioWidget()),
        new(QuickSettingsId, "WidgetQuickSettings", _ => "Wi-Fi · BT",
            DockWidgetCategory.MediaAndControls, "IconSliders", true,
            DockOrder: 17, Factory: () => new QuickSettingsWidget()),
        new(KeyboardLayoutId, "WidgetKeyboardLayout", _ => KeyboardLayoutService.GetActiveLayout().ShortCode,
            DockWidgetCategory.MediaAndControls, "IconKeyboard", false,
            DockOrder: 19, Factory: () => new KeyboardLayoutWidget()),

        // Productivity
        new(FocusId, "WidgetFocus", _ => "25:00",
            DockWidgetCategory.Productivity, "IconTarget", true,
            DockOrder: 2, Factory: () => new FocusWidget()),

        // Legacy alias kept only so old settings files stay valid; it is never a card or a dock slot.
        new(DateId, "WidgetClock", null, DockWidgetCategory.TimeAndWeather, "IconClock", false,
            CatalogVisible: false, SupportsLivePreview: false),
        // Keep saved widget lists valid after retiring the duplicate desktop control.
        new("ShowDesktop", "WidgetShowDesktop", null, DockWidgetCategory.Productivity, "IconMonitor", false,
            CatalogVisible: false, SupportsLivePreview: false)
    ];

    /// <summary>Every registered descriptor, including hidden legacy aliases.</summary>
    public static IReadOnlyList<DockWidgetDescriptor> AllDescriptors { get; } = Descriptors;

    /// <summary>All registered IDs, including hidden legacy aliases, used by settings validation.</summary>
    public static IReadOnlyCollection<string> SupportedIds { get; } =
        Descriptors.Select(item => item.Id).Distinct(StringComparer.Ordinal).ToArray();

    /// <summary>Cards shown on the Widgets settings page, in declaration order.</summary>
    public static IReadOnlyList<DockWidgetDescriptor> CatalogDescriptors { get; } =
        Descriptors.Where(item => item.CatalogVisible).ToArray();

    /// <summary>Dock widgets in the exact historical strip order; these are the only IDs with a dock slot.</summary>
    public static IReadOnlyList<DockWidgetDescriptor> LiveDescriptors { get; } =
        Descriptors.Where(item => item.Factory is not null && item.DockOrder is not null)
            .OrderBy(item => item.DockOrder!.Value).ToArray();

    public static bool TryGetDescriptor(string id, out DockWidgetDescriptor descriptor)
    {
        foreach (var item in Descriptors)
        {
            if (string.Equals(item.Id, id, StringComparison.OrdinalIgnoreCase))
            {
                descriptor = item;
                return true;
            }
        }
        descriptor = null!;
        return false;
    }

    private static readonly IReadOnlyDictionary<string, DockWidgetDescriptor> DescriptorsById =
        Descriptors.ToDictionary(item => item.Id, StringComparer.Ordinal);

    public IReadOnlyList<DockWidgetItem> CreateWidgets(IReadOnlyList<string>? selected) =>
        CatalogDescriptors.Where(definition => selected?.Contains(definition.Id, StringComparer.Ordinal) == true)
            .Select(CreateWidget)
            .ToArray();

    private DockWidgetItem CreateWidget(DockWidgetDescriptor definition) => definition.Id switch
    {
        StopwatchId => _stopwatch,
        CountdownId => _countdown,
        _ => new DockWidgetItem(definition.Id, UiText.Get(definition.LabelKey))
    };

    public IReadOnlyList<DockWidgetOption> CreateOptions(IReadOnlyList<string>? installed, IReadOnlyList<string>? selected) =>
        CatalogDescriptors.Select(definition => new DockWidgetOption(
            definition.Id,
            UiText.Get(definition.LabelKey),
            UiText.TryGet(definition.LabelKey + "Description", out var description) ? description : string.Empty,
            installed?.Contains(definition.Id, StringComparer.Ordinal) == true,
            selected?.Contains(definition.Id, StringComparer.Ordinal) == true,
            CreateWidget(definition),
            definition.Category))
            .ToArray();

    public static void Refresh(IEnumerable<DockWidgetItem> widgets, AppLanguage language)
    {
        var culture = language.Culture;
        foreach (var widget in widgets.Where(widget => DescriptorsById.ContainsKey(widget.Id)))
        {
            var definition = DescriptorsById[widget.Id];
            widget.Label = UiText.Get(definition.LabelKey);
            if (widget is ITimedDockWidget timed)
                timed.Refresh();
            else if (definition.Read is not null)
                widget.Value = definition.Read(culture);
        }
    }

    private static string ReadBatteryStatus()
    {
        if (!GetSystemPowerStatus(out var status) || status.BatteryLifePercent == UnknownBatteryLifePercent
            || status.BatteryFlag == NoSystemBattery)
            return UiText.Get("SystemDataUnavailable");
        var percent = $"{status.BatteryLifePercent}%";
        return (status.BatteryFlag & ChargingBatteryFlag) != 0
            ? $"{percent} · {UiText.Get("Charging")}"
            : percent;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct SystemPowerStatus
    {
        public byte ACLineStatus;
        public byte BatteryFlag;
        public byte BatteryLifePercent;
        public byte SystemStatusFlag;
        public uint BatteryLifeTime;
        public uint BatteryFullLifeTime;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetSystemPowerStatus(out SystemPowerStatus status);
}
