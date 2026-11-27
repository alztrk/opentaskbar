using System.IO;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

internal static class DynamicIslandChecks
{
    public static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        IReadOnlyList<DisplayInfo> displays =
        [
            new(1, "DISPLAY1", 1, "Primary", new Rect(0, 0, 1920, 1080), new Rect(0, 0, 1920, 1040), true, 96),
            new(2, "DISPLAY2", 2, "Secondary", new Rect(1920, 0, 1280, 720), new Rect(1920, 0, 1280, 680), false, 96)
        ];
        Check(DynamicIslandPolicy.ShouldCreateWindow(true, false)
            && !DynamicIslandPolicy.ShouldCreateWindow(false, false)
            && !DynamicIslandPolicy.ShouldCreateWindow(true, true)
            && DynamicIslandPolicy.WidgetsForDisplay([], null, "DISPLAY1").Count == 0,
            "An enabled island must stay available on an empty display unless disabled or focus mode is active.");
        IReadOnlyDictionary<string, string> placements = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            ["Media"] = "DISPLAY1",
            ["Clock"] = DockSettings.DynamicIslandAllDisplays
        };
        Check(DynamicIslandPolicy.IsAssignedToDisplay(placements, "Media", "DISPLAY1"),
            "A widget assigned to one display must appear there.");
        Check(!DynamicIslandPolicy.IsAssignedToDisplay(placements, "Media", "DISPLAY2"),
            "A single-display assignment must not duplicate the widget on another display.");
        Check(DynamicIslandPolicy.IsAssignedToDisplay(placements, "Clock", "DISPLAY1")
            && DynamicIslandPolicy.IsAssignedToDisplay(placements, "Clock", "DISPLAY2"),
            "An all-display assignment must appear on every connected display.");
        Check(DynamicIslandPolicy.IsAssignedToAnyDisplay(placements, "Clock", displays)
            && !DynamicIslandPolicy.IsAssignedToAnyDisplay(placements, "Clock", []),
            "All-display placement must count only while at least one display is connected.");
        Check(DynamicIslandPolicy.WidgetsForDisplay(["Media", "Clock"], placements, "DISPLAY2")
            .SequenceEqual(["Clock"]),
            "Each display must receive only its assigned widgets plus all-display widgets.");

        Check(DynamicIslandPolicy.ShouldShow(DynamicIslandVisibilityMode.Always, false, true, false),
            "Always mode must remain visible over ordinary app windows.");
        Check(!DynamicIslandPolicy.ShouldShow(DynamicIslandVisibilityMode.DesktopOnly, false, true, false)
            && DynamicIslandPolicy.ShouldShow(DynamicIslandVisibilityMode.DesktopOnly, true, true, false),
            "Desktop-only mode must follow whether the assigned display has an app window in front.");
        Check(!DynamicIslandPolicy.ShouldShow(DynamicIslandVisibilityMode.Always, true, true, true)
            && DynamicIslandPolicy.ShouldShow(DynamicIslandVisibilityMode.Always, true, false, true),
            "Full-screen suppression must be independently configurable.");
        Check(!DynamicIslandPolicy.ShouldShow((DynamicIslandVisibilityMode)99, true, false, false),
            "Unknown visibility modes must fail closed.");

        Check(DynamicIslandPolicy.OrderWidgets(
                ["Media", "Clock", "Weather"], ["Media"], ["Clock"], ["Weather"], ["Stopwatch"])
            .SequenceEqual(["Media", "Clock", "Weather"]),
            "Per-display order must take precedence while global and default orders fill missing items.");
        Check(DynamicIslandPolicy.MoveWidget(["Media", "Clock", "Weather"], "Clock", -1)
            .SequenceEqual(["Clock", "Media", "Weather"])
            && DynamicIslandPolicy.MoveWidget(["Media", "Clock", "Weather"], "Clock", 1)
                .SequenceEqual(["Media", "Weather", "Clock"])
            && DynamicIslandPolicy.MoveWidget(["Media"], "Media", -1).SequenceEqual(["Media"]),
            "Island order controls must move a widget one place and keep list edges stable.");

        Check(DynamicIslandPolicy.ClampWindowWidth(800, 420, 430, 960) == 388
            && DynamicIslandPolicy.ClampWindowWidth(1200, 1920, 360, 960) == 960,
            "The island must stay within monitor width and the configured maximum.");

        var path = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName() + ".json");
        try
        {
            var store = new DockSettingsStore(path);
            var freshSettings = store.Load();
            Check(freshSettings.Error is null && !freshSettings.Settings.DynamicIslandEnabled,
                "Fresh settings must keep the optional Dynamic Island disabled by default.");

            var legacy = JsonNode.Parse(JsonSerializer.Serialize(new DockSettings(
                DynamicIslandPlacements: new Dictionary<string, string> { ["Clock"] = "DISPLAY1" }),
                DockSettingsStore.SerializerOptions))!.AsObject();
            legacy.Remove(nameof(DockSettings.DynamicIslandEnabled));
            File.WriteAllText(path, legacy.ToJsonString(DockSettingsStore.SerializerOptions));
            var migrated = store.Load();
            Check(migrated.Error is null && migrated.Settings.DynamicIslandEnabled,
                "Existing widget-to-island assignments must preserve the user's previous opt-in during migration.");

            var configured = new DockSettings(
                Widgets: ["Clock", "Media"],
                InstalledWidgets: ["Clock", "Media"],
                DynamicIslandEnabled: true,
                DynamicIslandPlacements: new Dictionary<string, string>
                {
                    ["Clock"] = DockSettings.DynamicIslandAllDisplays,
                    ["Media"] = "DISPLAY1"
                },
                DynamicIslandVisibility: DynamicIslandVisibilityMode.DesktopOnly,
                DynamicIslandHideInFullscreen: true,
                DynamicIslandWidgetOrders: new Dictionary<string, IReadOnlyList<string>>
                {
                    ["DISPLAY1"] = ["Media", "Clock"]
                });
            Check(store.Save(configured) is null, "Valid island settings must save.");
            var loaded = store.Load();
            Check(loaded.Error is null
                && loaded.Settings.DynamicIslandEnabled
                && loaded.Settings.DynamicIslandVisibility == DynamicIslandVisibilityMode.DesktopOnly
                && loaded.Settings.DynamicIslandHideInFullscreen
                && loaded.Settings.DynamicIslandPlacements?.GetValueOrDefault("Clock") == DockSettings.DynamicIslandAllDisplays
                && loaded.Settings.DynamicIslandWidgetOrders?.GetValueOrDefault("DISPLAY1")?.SequenceEqual(["Media", "Clock"]) == true,
                "Island visibility, all-display assignments and per-display order must survive a settings round trip.");

            Check(store.Save(configured with
                {
                    DynamicIslandWidgetOrders = new Dictionary<string, IReadOnlyList<string>>
                    {
                        ["DISPLAY1"] = ["Media", "Media"]
                    }
                }) is not null,
                "Duplicate widget IDs must be rejected in island order settings.");
            Check(store.Save(configured with { DynamicIslandVisibility = (DynamicIslandVisibilityMode)99 }) is not null,
                "Unknown island visibility modes must be rejected.");
        }
        finally
        {
            if (File.Exists(path)) File.Delete(path);
        }

        return checks;
    }
}
