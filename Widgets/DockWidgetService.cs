using System.Globalization;
using System.Runtime.InteropServices;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

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
    public const string DesktopsId = "Desktops";
    public const string TrayId = "Tray";
    public const string WeatherId = "Weather";
    public const string BatteryId = "Battery";
    public const string StopwatchId = "Stopwatch";
    public const string CountdownId = "Countdown";
    public const string SearchId = "Search";
    public const string NetSpeedId = "NetSpeed";
    public const string BluetoothBatteryId = "BluetoothBattery";

    private sealed record WidgetDefinition(string Id, string LabelKey, Func<CultureInfo, string>? Read);

    public sealed record AiUsageWidgetDefinition(string Id, AiProviders Provider, string LabelKey);

    /// <summary>Every AI provider is its own widget so each can be added, ordered and removed independently.</summary>
    public static IReadOnlyList<AiUsageWidgetDefinition> AiUsageWidgets { get; } =
    [
        new("AiUsageClaude", AiProviders.Claude, "WidgetAiUsageClaude"),
        new("AiUsageCodex", AiProviders.Codex, "WidgetAiUsageCodex"),
        new("AiUsageAntigravity", AiProviders.Antigravity, "WidgetAiUsageAntigravity"),
        new("AiUsageOpenCode", AiProviders.OpenCode, "WidgetAiUsageOpenCode"),
        new("AiUsageCopilot", AiProviders.Copilot, "WidgetAiUsageCopilot"),
        new("AiUsageDeepSeek", AiProviders.DeepSeek, "WidgetAiUsageDeepSeek"),
        new("AiUsageOpenRouter", AiProviders.OpenRouter, "WidgetAiUsageOpenRouter")
    ];

    private static readonly WidgetDefinition[] Definitions =
    [
        new(ClockId, "WidgetClock", culture => DateTime.Now.ToString("HH:mm", culture)),
        new(MediaId, "WidgetMedia", _ => "▶ · 02:15"),
        new(AudioId, "WidgetAudio", _ => "100%"),
        new(QuickSettingsId, "WidgetQuickSettings", _ => "Wi-Fi · BT"),
        new(SystemStatsId, "WidgetSystemStats", _ => "CPU · RAM"),
        new(FocusId, "WidgetFocus", _ => "25:00"),
        new(WeatherId, "WidgetWeather", _ => "21°"),
        new(NetSpeedId, "WidgetNetSpeed", _ => "↓ 0 B/s · ↑ 0 B/s"),
        new(BluetoothBatteryId, "WidgetBluetoothBattery", _ => "53% · 🔋"),
        .. AiUsageWidgets.Select(widget => new WidgetDefinition(widget.Id, widget.LabelKey, null)),
        new(DesktopsId, "WidgetDesktops", _ => "1 / 2"),
        new(TrayId, "WidgetTray", _ => "▲"),
        new(BatteryId, "WidgetBattery", _ => ReadBatteryStatus()),
        new("Storage", "WidgetStorage", SystemWidgetData.ReadStorage),
        new("DayProgress", "WidgetDayProgress", culture =>
            (DateTime.Now.TimeOfDay.TotalSeconds / TimeSpan.FromDays(1).TotalSeconds).ToString("P0", culture)),
        new(SearchId, "WidgetLauncher", _ => "Win+S"),
        new(StopwatchId, "WidgetStopwatch", null),
        new(CountdownId, "WidgetCountdown", null)
    ];

    public static IReadOnlyCollection<string> SupportedIds { get; } =
        Definitions.Select(item => item.Id).Append(DateId).Distinct(StringComparer.Ordinal).ToArray();
    private static readonly IReadOnlyDictionary<string, WidgetDefinition> DefinitionsById =
        Definitions.ToDictionary(item => item.Id, StringComparer.Ordinal);

    public IReadOnlyList<DockWidgetItem> CreateWidgets(IReadOnlyList<string>? selected) =>
        Definitions.Where(definition => selected?.Contains(definition.Id, StringComparer.Ordinal) == true)
            .Select(CreateWidget)
            .ToArray();

    private DockWidgetItem CreateWidget(WidgetDefinition definition) => definition.Id switch
    {
        StopwatchId => _stopwatch,
        CountdownId => _countdown,
        _ => new DockWidgetItem(definition.Id, UiText.Get(definition.LabelKey))
    };

    public IReadOnlyList<DockWidgetOption> CreateOptions(IReadOnlyList<string>? installed, IReadOnlyList<string>? selected) =>
        Definitions.Select(definition => new DockWidgetOption(
            definition.Id,
            UiText.Get(definition.LabelKey),
            UiText.Get(definition.LabelKey + "Description"),
            installed?.Contains(definition.Id, StringComparer.Ordinal) == true,
            selected?.Contains(definition.Id, StringComparer.Ordinal) == true,
            CreateWidget(definition)))
            .ToArray();

    public static void Refresh(IEnumerable<DockWidgetItem> widgets, AppLanguage language)
    {
        var culture = language.Culture;
        foreach (var widget in widgets)
        {
            if (DefinitionsById.TryGetValue(widget.Id, out var definition))
            {
                widget.Label = UiText.Get(definition.LabelKey);
                if (widget is ITimedDockWidget timed)
                    timed.Refresh();
                else if (definition.Read is not null)
                    widget.Value = definition.Read(culture);
            }
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
