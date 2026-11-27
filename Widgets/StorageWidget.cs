using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Widgets;

namespace OpenTaskbar.Widgets;

public readonly record struct DriveStorageInfo(string Letter, long FreeBytes, long TotalBytes);

/// <summary>
/// V19 compact horizontal multi-drive storage widget: "C: 213 / 476 GB | D: 1.2 / 1.8 TB".
/// Reads real fixed drives asynchronously off the UI thread with no progress bars or extra copy.
/// </summary>
public sealed class StorageWidget : DockWidget
{
    public static string Id => "Storage";

    private const long PollIntervalMs = 30000;
    private const double BytesPerGb = 1024d * 1024d * 1024d;
    private const double BytesPerTb = BytesPerGb * 1024d;

    private readonly Border _card;
    private readonly StackPanel _drivesPanel;
    private IReadOnlyList<DriveStorageInfo> _drives = [];
    private long _lastPollMs;
    private bool _polling;
    private int _generation;

    internal Func<IReadOnlyList<DriveStorageInfo>> DriveReader { get; set; } = ReadFixedDrives;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 35;
    public override bool SupportsStaticPreview => true;
    public override double FullWidthEstimate => Math.Max(110, Math.Max(1, _drives.Count) * 108);
    public override double CompactWidthEstimate => 78;

    public override UIElement BuildDynamicIslandExpandedContent()
    {
        if (_drives.Count == 0)
        {
            return DynamicIslandStatusPanel.Create(
                UiText.Get("WidgetStorage"), "—", UiText.Get("SystemDataUnavailable"), null,
                Color.FromRgb(0xBB, 0xD3, 0xFA));
        }

        var culture = Settings.Language.Culture;
        var rows = _drives.Select(drive => new DynamicIslandStatusRow(
            drive.Letter,
            FormatCapacityPair(Math.Max(0, drive.TotalBytes - drive.FreeBytes), drive.TotalBytes, culture),
            FormatFreeCapacity(drive.FreeBytes, drive.TotalBytes, culture),
            drive.TotalBytes <= 0 ? 0 : Math.Clamp((double)(drive.TotalBytes - drive.FreeBytes) / drive.TotalBytes, 0, 1)))
            .ToArray();
        return DynamicIslandStatusPanel.CreateRows(
            UiText.Get("WidgetStorage"), rows, Color.FromRgb(0xBB, 0xD3, 0xFA));
    }

    public StorageWidget()
    {
        _drivesPanel = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center
        };

        _card = new Border
        {
            Padding = new Thickness(10, 0, 10, 0),
            Child = _drivesPanel
        };
        _card.SetResourceReference(StyleProperty, "WidgetCardBorder");

        Content = _card;
        HasData = true;
    }

    protected override void OnActivated()
    {
        _lastPollMs = 0;
        _generation++;
        QueuePoll();
    }

    protected override void OnDeactivated()
    {
        _generation++;
        _polling = false;
    }

    protected override void OnCompactChanged(bool compact) => Render();
    protected override void OnHoverExpandedChanged(bool expanded) => Render();
    protected override void OnLanguageChanged() => Render();

    protected override void OnRenderPreview()
    {
        _drives = DriveReader();
        Render();
    }

    protected override void OnTick(DateTime now)
    {
        var tick = Environment.TickCount64;
        if (_lastPollMs == 0 || tick - _lastPollMs >= PollIntervalMs)
        {
            _lastPollMs = tick;
            QueuePoll();
        }
    }

    internal void ApplyDrives(IReadOnlyList<DriveStorageInfo> drives)
    {
        _drives = drives;
        Render();
    }

    private void QueuePoll()
    {
        if (_polling) return;
        _polling = true;
        var generation = _generation;
        var reader = DriveReader;
        _ = Task.Run(reader).ContinueWith(task => Dispatcher.BeginInvoke(() =>
        {
            if (generation != _generation) return;
            _polling = false;
            if (!IsActive || !task.IsCompletedSuccessfully) return;
            _drives = task.Result;
            Render();
        }), TaskScheduler.Default);
    }

    private void Render()
    {
        _drivesPanel.Children.Clear();
        var culture = Settings.Language.Culture;

        if (_drives.Count == 0)
        {
            var empty = new TextBlock
            {
                Text = "—",
                FontSize = 11.5,
                VerticalAlignment = VerticalAlignment.Center
            };
            empty.SetResourceReference(StyleProperty, "WidgetCaption");
            _drivesPanel.Children.Add(empty);
            _card.ToolTip = UiText.Get("SystemDataUnavailable");
            AutomationProperties.SetName(_card, $"{UiText.Get("WidgetStorage")}: {UiText.Get("SystemDataUnavailable")}");
            RaiseContentWidthChanged();
            return;
        }

        var compact = IsCompact && !IsHoverExpanded;
        _card.Padding = compact ? new Thickness(8, 0, 8, 0) : new Thickness(10, 0, 10, 0);
        var visibleDrives = compact ? _drives.Take(1).ToList() : _drives;
        var tooltipLines = new List<string>(_drives.Count);

        for (var i = 0; i < visibleDrives.Count; i++)
        {
            if (i > 0)
            {
                var sep = new Border
                {
                    Width = 1,
                    Height = 13,
                    Margin = new Thickness(8, 0, 8, 0),
                    VerticalAlignment = VerticalAlignment.Center
                };
                sep.SetResourceReference(Border.BackgroundProperty, "StrokeSubtle");
                _drivesPanel.Children.Add(sep);
            }

            var drive = visibleDrives[i];
            var capacityText = compact
                ? FormatFreeCapacity(drive.FreeBytes, drive.TotalBytes, culture)
                : FormatCapacityPair(drive.FreeBytes, drive.TotalBytes, culture);

            var entry = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                VerticalAlignment = VerticalAlignment.Center
            };

            var letter = new TextBlock
            {
                Text = drive.Letter,
                FontSize = 11,
                FontWeight = FontWeights.SemiBold,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(0, 0, 4, 0)
            };
            letter.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");

            var capacity = new TextBlock
            {
                Text = capacityText,
                FontSize = 11.5,
                FontWeight = FontWeights.SemiBold,
                VerticalAlignment = VerticalAlignment.Center,
                Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
            };
            capacity.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

            entry.Children.Add(letter);
            entry.Children.Add(capacity);
            _drivesPanel.Children.Add(entry);
        }

        foreach (var drive in _drives)
        {
            tooltipLines.Add($"{drive.Letter} {FormatCapacityPair(drive.FreeBytes, drive.TotalBytes, culture)}");
        }

        _card.ToolTip = string.Join("\n", tooltipLines);
        AutomationProperties.SetName(_card, $"{UiText.Get("WidgetStorage")}: {string.Join(", ", tooltipLines)}");
        RaiseContentWidthChanged();
    }

    internal static string FormatCapacityPair(long freeBytes, long totalBytes, CultureInfo culture)
    {
        var free = Math.Max(0, freeBytes);
        var total = Math.Max(1, totalBytes);
        if (total >= BytesPerTb * 0.95)
        {
            var freeTb = free / BytesPerTb;
            var totalTb = total / BytesPerTb;
            return $"{freeTb.ToString("0.0", culture)} / {totalTb.ToString("0.0", culture)} TB";
        }

        var freeGb = Math.Round(free / BytesPerGb);
        var totalGb = Math.Round(total / BytesPerGb);
        return $"{freeGb.ToString("0", culture)} / {totalGb.ToString("0", culture)} GB";
    }

    internal static string FormatFreeCapacity(long freeBytes, long totalBytes, CultureInfo culture)
    {
        var free = Math.Max(0, freeBytes);
        var total = Math.Max(1, totalBytes);
        if (total >= BytesPerTb * 0.95)
        {
            var freeTb = free / BytesPerTb;
            return $"{freeTb.ToString("0.0", culture)} TB";
        }

        var freeGb = Math.Round(free / BytesPerGb);
        return $"{freeGb.ToString("0", culture)} GB";
    }

    private static IReadOnlyList<DriveStorageInfo> ReadFixedDrives()
    {
        var results = new List<DriveStorageInfo>();
        try
        {
            foreach (var drive in DriveInfo.GetDrives())
            {
                try
                {
                    if (drive.DriveType != DriveType.Fixed || !drive.IsReady) continue;
                    var letter = drive.Name.TrimEnd('\\');
                    if (string.IsNullOrEmpty(letter)) continue;
                    results.Add(new DriveStorageInfo(letter, drive.AvailableFreeSpace, drive.TotalSize));
                }
                catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or DriveNotFoundException)
                {
                    // Skip disconnected, locked, or inaccessible volumes safely.
                }
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            AppLog.Warning($"Storage enumeration failed: {ex.Message}");
        }

        return results;
    }
}
