using System.Windows;
using System.Windows.Controls.Primitives;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Single source of truth for dock geometry and motion. XAML reads these through x:Static in Design/Tokens/Layout.xaml.
/// </summary>
public static class DockMetrics
{
    /// <summary>
    /// Artwork is never drawn wider or taller than its dock cell. Snapped extraction sizes can be larger than the
    /// cell at some DPI/profile combinations; clipping that overshoot is what made icons look cut on the right and
    /// bottom instead of sitting evenly in the button.
    /// </summary>
    public static double AppIconDipSize(double iconDip, int preferredPixels, double dpiScale) =>
        Math.Min(iconDip, preferredPixels / Math.Max(1.0, dpiScale));

    // Vertical layout (DIP).
    public const double Headroom = 24;
    public const double ShelfHeight = 66;
    public const double WindowHeight = Headroom + ShelfHeight;
    public const double BottomGap = 6;
    public const double SideInset = 12;
    public const double FullWidthSideInset = 6;
    public const double EdgeTriggerHeight = 5;

    // Cells and icons (DIP).
    public const double CellSize = 52;
    public const double MinCellSize = 24;
    public const double IconSize = 42;

    // Magnification. Icons grow to 1.5x and push neighbours aside along the dock axis.
    public const double HoverScale = 1.5;
    public const double InfluenceRadius = 135;

    // Previews.
    public const double PreviewGap = 18;

    /// <summary>
    /// Visible gap (DIP) between a flyout and the dock edge. Kept in the 5–10 px band and recalculated from the
    /// current size profile in <see cref="MainWindow.ApplySizeProfile"/>, so changing the dock height/scale keeps
    /// the spacing consistent instead of letting it drift with a fixed, hard-coded offset.
    /// </summary>
    public const double FlyoutGapMin = 5;
    public const double FlyoutGapMax = 10;
    public const double FlyoutContentInset = 4;

    /// <summary>Desired visible gap (DIP) from the dock edge to the flyout's visible frame.</summary>
    public const double DesiredFlyoutGap = 7;

    private static double _flyoutGap = DesiredFlyoutGap;

    /// <summary>
    /// Current placement gap (DIP) from the anchor to the popup window. All flyout placement callbacks read this
    /// single value; it is recomputed from the dock profile in <see cref="MainWindow.ApplySizeProfile"/>.
    /// </summary>
    public static double FlyoutGap
    {
        get => _flyoutGap;
        set => _flyoutGap = double.IsFinite(value) ? Math.Clamp(value, FlyoutGapMin, FlyoutGapMax) : DesiredFlyoutGap;
    }

    /// <summary>
    /// Derives the placement gap from the dock profile so the visible gap to the dock stays near
    /// <see cref="DesiredFlyoutGap"/> (and inside the 5–10 px band). The anchor sits <c>VerticalPadding + 1</c>
    /// inside the dock frame, which grows with dock size/scale; subtracting the popup's bottom content inset makes
    /// the resulting gap measured to the visible flyout frame instead of the popup window edge.
    /// </summary>
    public static double ComputeFlyoutGap(DockSizeProfile profile)
    {
        var anchorInset = profile.VerticalPadding + 1;
        return Math.Clamp(anchorInset + DesiredFlyoutGap - FlyoutContentInset, FlyoutGapMin, FlyoutGapMax);
    }

    // Motion (ms).
    public const double AutoHideDelayMs = 400;
    public const double RevealMs = 220;
    public const double HideMs = 220;
    public const double LaunchBounceMs = 420;
    public const double LaunchBounceHeight = 12;
    public const double HideMinimumOffset = 72;

    public static Thickness Inset { get; } = new(SideInset, 0, SideInset, BottomGap);
    public static Thickness Padding { get; } = new(10, 4, 10, 4);

    public static CustomPopupPlacementCallback TooltipPlacementCallback { get; } = PlaceDockTooltip;

    public static CustomPopupPlacement[] PlaceDockTooltip(Size popupSize, Size targetSize, Point offset)
    {
        var gap = FlyoutGap;
        return DockFlyout.Edge switch
        {
            DockEdge.Left =>
            [
                new CustomPopupPlacement(new Point(targetSize.Width + gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.None),
                new CustomPopupPlacement(new Point(-popupSize.Width - gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.None)
            ],
            DockEdge.Right =>
            [
                new CustomPopupPlacement(new Point(-popupSize.Width - gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.None),
                new CustomPopupPlacement(new Point(targetSize.Width + gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.None)
            ],
            _ =>
            [
                new CustomPopupPlacement(new Point((targetSize.Width - popupSize.Width) / 2, -popupSize.Height - gap), PopupPrimaryAxis.None),
                new CustomPopupPlacement(new Point((targetSize.Width - popupSize.Width) / 2, targetSize.Height + gap), PopupPrimaryAxis.None)
            ]
        };
    }

    /// <summary>Natural widget card height; smaller dock sizes scale the widget strip down from it.</summary>
    public const double WidgetDesignHeight = 46;

    /// <summary>
    /// Geometry for each dock size and user scale factor (0.75–1.50). Surface = cell + 2 × vertical padding + 2 (border);
    /// it plus the bottom gap fits the shelf. Compact at 100% matches the Windows 11 taskbar height (48 DIP).
    /// </summary>
    public static DockSizeProfile For(Models.DockSize size, double scale = 1.0)
    {
        var clamped = double.IsFinite(scale) ? Math.Clamp(scale, 0.75, 1.5) : 1.0;
        var baseProfile = size switch
        {
            Models.DockSize.Compact => new DockSizeProfile(Shelf: 48, BottomGap: 3, Cell: 38, VerticalPadding: 2, Icon: 28, Radius: 16),
            Models.DockSize.Medium => new DockSizeProfile(Shelf: 58, BottomGap: 5, Cell: 44, VerticalPadding: 3, Icon: 34, Radius: 19),
            _ => new DockSizeProfile(Shelf: ShelfHeight, BottomGap: BottomGap, Cell: 50, VerticalPadding: 4, Icon: 38, Radius: 22)
        };
        if (Math.Abs(clamped - 1.0) < 0.001) return baseProfile;
        var cell = Math.Round(baseProfile.Cell * clamped);
        var icon = Math.Round(baseProfile.Icon * clamped);
        var verticalPadding = Math.Max(2, Math.Round(baseProfile.VerticalPadding * clamped));
        var bottomGap = Math.Max(2, Math.Round(baseProfile.BottomGap * clamped));
        var surfaceHeight = cell + verticalPadding * 2 + 2;
        var shelf = Math.Max(36, surfaceHeight + bottomGap + 1);
        var radius = Math.Clamp(Math.Round(baseProfile.Radius * clamped), 10, 30);
        return new DockSizeProfile(Shelf: shelf, BottomGap: bottomGap, Cell: cell, VerticalPadding: verticalPadding, Icon: icon, Radius: radius);
    }
}

public readonly record struct DockSizeProfile(double Shelf, double BottomGap, double Cell, double VerticalPadding, double Icon, double Radius)
{
    public double SurfaceHeight => Cell + VerticalPadding * 2 + 2;
    public double WidgetScale => Math.Clamp((Cell - 2) / DockMetrics.WidgetDesignHeight, 0.45, 1.35);
}
