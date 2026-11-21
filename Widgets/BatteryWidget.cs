using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Shapes;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// V19 horizontal liquid-fill battery indicator. Transparent by default with no icon or outer border;
/// a single continuous liquid silhouette fills to the exact charge percentage and gently morphs its
/// trailing wave boundary while charging.
/// </summary>
public sealed class BatteryWidget : DockWidget
{
    public static string Id => "Battery";

    private const double MinCardWidth = 96;
    private const double WaveHeight = 46;
    private const long PollIntervalMs = 15000;

    private readonly Border _card;
    private readonly Grid _surface;
    private readonly Path _liquidPath;
    private readonly PathFigure _figure;
    private readonly LineSegment _topEdge;
    private readonly BezierSegment _upperWave;
    private readonly BezierSegment _lowerWave;
    private readonly LineSegment _bottomLeft;
    private readonly TextBlock _percentShadow;
    private readonly TextBlock _percentText;

    private BatteryStatus _status;
    private bool _hasSample;
    private bool _isAnimatingWave;
    private long _lastPollMs;
    private int _renderedPercent = -1;
    private bool _renderedCharging;
    private bool _renderedPresent;

    internal Func<BatteryStatus> BatteryReader { get; set; } = SystemStats.SampleBattery;
    internal Action OpenPowerSettingsAction { get; set; } = () => ShellShortcuts.OpenUri("ms-settings:batterysaver");

    public override bool SupportsStaticPreview => true;
    public override double FullWidthEstimate => MinCardWidth + 4;
    public override double CompactWidthEstimate => MinCardWidth + 4;

    public BatteryWidget()
    {
        _topEdge = new LineSegment(new Point(0, 0), isStroked: false);
        _upperWave = new BezierSegment(new Point(0, 8), new Point(0, 16), new Point(0, 23), isStroked: false);
        _lowerWave = new BezierSegment(new Point(0, 31), new Point(0, 39), new Point(0, WaveHeight), isStroked: false);
        _bottomLeft = new LineSegment(new Point(0, WaveHeight), isStroked: false);

        _figure = new PathFigure
        {
            StartPoint = new Point(0, 0),
            IsClosed = true,
            IsFilled = true,
            Segments = { _topEdge, _upperWave, _lowerWave, _bottomLeft }
        };

        _liquidPath = new Path
        {
            Data = new PathGeometry { Figures = { _figure } },
            Stretch = Stretch.None,
            IsHitTestVisible = false,
            Visibility = Visibility.Collapsed
        };

        _percentShadow = new TextBlock
        {
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Foreground = new SolidColorBrush(Color.FromArgb(0x58, 0x00, 0x00, 0x00)),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 1, 0, 0),
            IsHitTestVisible = false,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };

        _percentText = new TextBlock
        {
            Text = "—",
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            IsHitTestVisible = false,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        _percentText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");

        var hoverOverlay = new Border
        {
            CornerRadius = new CornerRadius(10),
            Opacity = 0,
            IsHitTestVisible = false
        };
        hoverOverlay.SetResourceReference(Border.BackgroundProperty, "SurfaceHover");

        _surface = new Grid
        {
            MinWidth = MinCardWidth,
            ClipToBounds = true,
            Background = Brushes.Transparent
        };
        _surface.Children.Add(_liquidPath);
        _surface.Children.Add(hoverOverlay);
        _surface.Children.Add(_percentShadow);
        _surface.Children.Add(_percentText);
        _surface.SizeChanged += (_, _) => UpdateLiquidGeometry(forceAnimationRestart: false);

        _card = new Border
        {
            MinWidth = MinCardWidth,
            Padding = new Thickness(0),
            ClipToBounds = true,
            Cursor = Cursors.Hand,
            Child = _surface
        };
        _card.SetResourceReference(StyleProperty, "WidgetCardBorder");
        _card.Padding = new Thickness(0);
        // Hover is rendered by hoverOverlay above _liquidPath so the opaque fill never hides it
        // and the unfilled portion never double-stacks with WidgetCardBorder's Background.
        _card.Background = new SolidColorBrush(Colors.Transparent);
        _card.MouseEnter += (_, _) => hoverOverlay.BeginAnimation(OpacityProperty,
            new DoubleAnimation(1, TimeSpan.FromMilliseconds(160)) { EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut } });
        _card.MouseLeave += (_, _) => hoverOverlay.BeginAnimation(OpacityProperty,
            new DoubleAnimation(0, TimeSpan.FromMilliseconds(160)) { EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut } });
        _card.MouseLeftButtonUp += (_, _) => OpenPowerSettingsAction();

        Content = _card;
        HasData = true;
    }

    protected override void OnActivated()
    {
        _lastPollMs = 0;
        SampleAndRender();
    }

    protected override void OnDeactivated()
    {
        StopWaveAnimation();
    }

    protected override void OnDisposed()
    {
        StopWaveAnimation();
    }

    protected override void OnSettingsChanged()
    {
        if (IsActive) UpdateLiquidGeometry(forceAnimationRestart: true);
    }

    protected override void OnLanguageChanged()
    {
        if (IsActive) Render(force: true);
    }

    protected override void OnRenderPreview()
    {
        SampleAndRender();
    }

    protected override void OnTick(DateTime now)
    {
        var tick = Environment.TickCount64;
        if (_lastPollMs == 0 || tick - _lastPollMs >= PollIntervalMs)
        {
            _lastPollMs = tick;
            SampleAndRender();
        }
    }

    internal void ApplyStatus(BatteryStatus status)
    {
        _status = status;
        _hasSample = true;
        Render(force: false);
    }

    private void SampleAndRender()
    {
        _status = BatteryReader();
        _hasSample = true;
        Render(force: false);
    }

    private void Render(bool force)
    {
        if (!_hasSample) return;

        var present = _status.Present;
        var percent = Math.Clamp(_status.Percent, 0, 100);
        var charging = present && _status.Charging;

        if (!force && present == _renderedPresent && percent == _renderedPercent && charging == _renderedCharging)
            return;

        _renderedPresent = present;
        _renderedPercent = percent;
        _renderedCharging = charging;

        if (!present)
        {
            StopWaveAnimation();
            _liquidPath.Visibility = Visibility.Collapsed;
            _percentShadow.Visibility = Visibility.Collapsed;
            _percentText.Text = "—";
            _percentText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            var unavailable = UiText.Get("SystemDataUnavailable");
            _card.ToolTip = $"{UiText.Get("WidgetBattery")}: {unavailable}";
            AutomationProperties.SetName(_card, $"{UiText.Get("WidgetBattery")}: {unavailable}");
            return;
        }

        _liquidPath.Visibility = Visibility.Visible;
        _liquidPath.Fill = CreatePaletteBrush(percent);

        var percentLabel = $"{percent.ToString(CultureInfo.InvariantCulture)}%";
        _percentText.Text = percentLabel;
        _percentShadow.Text = percentLabel;
        _percentShadow.Visibility = Visibility.Visible;
        _percentText.Foreground = Brushes.White;

        var labelKey = charging ? "BatteryCharging" : "Battery";
        var summary = $"{UiText.Get(labelKey)}: {UiText.Format("Percent", percent)}";
        _card.ToolTip = summary;
        AutomationProperties.SetName(_card, summary);

        UpdateLiquidGeometry(forceAnimationRestart: true);
    }

    internal static (Color Top, Color Bottom) PaletteForLevel(int percent) => percent switch
    {
        <= 20 => (Color.FromRgb(0xB2, 0x60, 0x67), Color.FromRgb(0x98, 0x50, 0x5A)),
        <= 50 => (Color.FromRgb(0xB9, 0x95, 0x63), Color.FromRgb(0x96, 0x76, 0x48)),
        _ => (Color.FromRgb(0x58, 0xA8, 0x9C), Color.FromRgb(0x3E, 0x82, 0x7E))
    };

    private static LinearGradientBrush CreatePaletteBrush(int percent)
    {
        var (top, bottom) = PaletteForLevel(percent);
        var brush = new LinearGradientBrush(top, bottom, new Point(0, 0), new Point(0, 1));
        brush.Freeze();
        return brush;
    }

    private void UpdateLiquidGeometry(bool forceAnimationRestart)
    {
        if (!_renderedPresent) return;

        var width = _surface.ActualWidth > 1 ? _surface.ActualWidth : MinCardWidth;
        var height = _surface.ActualHeight > 1 ? _surface.ActualHeight : WaveHeight;
        _surface.Clip = new RectangleGeometry(new Rect(0, 0, width, height), 10, 10);
        var fillRatio = Math.Clamp(_renderedPercent, 2, 100) / 100.0;
        var edgeX = width * fillRatio;

        var shouldAnimate = _renderedCharging
            && IsActive
            && Settings.AnimationsEnabled
            && SystemParameters.ClientAreaAnimation
            && _renderedPercent < 100;

        if (!shouldAnimate)
        {
            StopWaveAnimation();
            SetStaticWave(edgeX, height);
            return;
        }

        if (_isAnimatingWave && !forceAnimationRestart) return;

        StartWaveAnimation(edgeX, height);
    }

    private void SetStaticWave(double edgeX, double height)
    {
        var midY = height * 0.5;
        _topEdge.Point = new Point(edgeX, 0);
        _upperWave.Point1 = new Point(edgeX, height * 0.16);
        _upperWave.Point2 = new Point(edgeX, height * 0.34);
        _upperWave.Point3 = new Point(edgeX, midY);
        _lowerWave.Point1 = new Point(edgeX, height * 0.68);
        _lowerWave.Point2 = new Point(edgeX, height * 0.84);
        _lowerWave.Point3 = new Point(edgeX, height);
        _bottomLeft.Point = new Point(0, height);
    }

    private void StartWaveAnimation(double edgeX, double height)
    {
        StopWaveAnimation();
        _isAnimatingWave = true;

        var midY = height * 0.5;
        var halfCycle = TimeSpan.FromMilliseconds(1850);
        var ease = new SineEase { EasingMode = EasingMode.EaseInOut };

        PointAnimation MakeAnim(Point from, Point to) => new(from, to, halfCycle)
        {
            AutoReverse = true,
            RepeatBehavior = RepeatBehavior.Forever,
            EasingFunction = ease
        };

        // Stationary liquid body; only the trailing right-hand boundary gently morphs.
        _bottomLeft.Point = new Point(0, height);
        _topEdge.BeginAnimation(LineSegment.PointProperty,
            MakeAnim(new Point(edgeX - 1.6, 0), new Point(edgeX - 1.0, 0)));
        _upperWave.BeginAnimation(BezierSegment.Point1Property,
            MakeAnim(new Point(edgeX + 1.4, height * 0.16), new Point(edgeX - 2.2, height * 0.16)));
        _upperWave.BeginAnimation(BezierSegment.Point2Property,
            MakeAnim(new Point(edgeX - 2.2, height * 0.34), new Point(edgeX + 1.5, height * 0.34)));
        _upperWave.BeginAnimation(BezierSegment.Point3Property,
            MakeAnim(new Point(edgeX - 0.5, midY), new Point(edgeX - 0.6, midY)));
        _lowerWave.BeginAnimation(BezierSegment.Point1Property,
            MakeAnim(new Point(edgeX + 1.4, height * 0.68), new Point(edgeX - 2.2, height * 0.68)));
        _lowerWave.BeginAnimation(BezierSegment.Point2Property,
            MakeAnim(new Point(edgeX - 1.9, height * 0.84), new Point(edgeX + 1.4, height * 0.84)));
        _lowerWave.BeginAnimation(BezierSegment.Point3Property,
            MakeAnim(new Point(edgeX - 1.0, height), new Point(edgeX - 1.6, height)));
    }

    private void StopWaveAnimation()
    {
        if (!_isAnimatingWave) return;
        _isAnimatingWave = false;
        _topEdge.BeginAnimation(LineSegment.PointProperty, null);
        _upperWave.BeginAnimation(BezierSegment.Point1Property, null);
        _upperWave.BeginAnimation(BezierSegment.Point2Property, null);
        _upperWave.BeginAnimation(BezierSegment.Point3Property, null);
        _lowerWave.BeginAnimation(BezierSegment.Point1Property, null);
        _lowerWave.BeginAnimation(BezierSegment.Point2Property, null);
        _lowerWave.BeginAnimation(BezierSegment.Point3Property, null);
    }
}
