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
/// V19 horizontal liquid-fill battery indicator. Displays the real charge percentage with a
/// level-colored liquid fill, a crisp charging bolt icon, a continuous scrolling wave boundary,
/// and a soft left-to-right charging sheen while charging.
/// </summary>
public sealed class BatteryWidget : DockWidget
{
    public static string Id => "Battery";

    private const double MinCardWidth = 96;
    private const double WaveHeight = 46;
    private const double WaveAmplitude = 3.2;
    private const double SheenWidth = 54;
    private const long PollIntervalMs = 15000;

    private readonly Border _card;
    private readonly Grid _surface;
    private readonly Border _trackLayer;
    private readonly Rectangle _liquidFill;
    private readonly TranslateTransform _waveTransform;
    private readonly Grid _sheenClipHost;
    private readonly Border _chargeSheen;
    private readonly TranslateTransform _sheenTransform;
    private readonly ContentControl _boltShadow;
    private readonly ContentControl _boltIcon;
    private readonly TextBlock _percentShadow;
    private readonly TextBlock _percentText;

    private BatteryStatus _status;
    private bool _hasSample;
    private bool _isAnimatingCharging;
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
        _trackLayer = new Border
        {
            CornerRadius = new CornerRadius(10),
            Opacity = 0.28,
            IsHitTestVisible = false,
            Visibility = Visibility.Collapsed
        };
        _trackLayer.SetResourceReference(Border.BackgroundProperty, "WidgetTrack");

        _waveTransform = new TranslateTransform(0, 0);
        _liquidFill = new Rectangle
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Stretch,
            IsHitTestVisible = false,
            Visibility = Visibility.Collapsed
        };

        _sheenTransform = new TranslateTransform(-SheenWidth - 28, 0);
        var sheenBrush = new LinearGradientBrush
        {
            StartPoint = new Point(0, 0),
            EndPoint = new Point(1, 0),
            GradientStops = new GradientStopCollection
            {
                new GradientStop(Color.FromArgb(0x00, 0xFF, 0xFF, 0xFF), 0.00),
                new GradientStop(Color.FromArgb(0x00, 0xFF, 0xFF, 0xFF), 0.10),
                new GradientStop(Color.FromArgb(0x16, 0xFF, 0xFF, 0xFF), 0.30),
                new GradientStop(Color.FromArgb(0x34, 0xFF, 0xFF, 0xFF), 0.50),
                new GradientStop(Color.FromArgb(0x16, 0xFF, 0xFF, 0xFF), 0.70),
                new GradientStop(Color.FromArgb(0x00, 0xFF, 0xFF, 0xFF), 0.90),
                new GradientStop(Color.FromArgb(0x00, 0xFF, 0xFF, 0xFF), 1.00)
            }
        };
        sheenBrush.Freeze();

        var sheenTransforms = new TransformGroup();
        sheenTransforms.Children.Add(new SkewTransform(-22, 0));
        sheenTransforms.Children.Add(_sheenTransform);

        _chargeSheen = new Border
        {
            Width = SheenWidth,
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Stretch,
            Background = sheenBrush,
            RenderTransform = sheenTransforms,
            IsHitTestVisible = false,
            Visibility = Visibility.Collapsed
        };

        _sheenClipHost = new Grid
        {
            ClipToBounds = true,
            IsHitTestVisible = false
        };
        _sheenClipHost.Children.Add(_chargeSheen);

        _boltShadow = new ContentControl
        {
            Width = 11,
            Height = 11,
            Margin = new Thickness(0, 1, 3, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = new SolidColorBrush(Color.FromArgb(0x58, 0x00, 0x00, 0x00)),
            IsHitTestVisible = false,
            Visibility = Visibility.Collapsed
        };
        _boltShadow.SetResourceReference(StyleProperty, "WidgetGlyph");
        _boltShadow.SetResourceReference(ContentProperty, "IconZap");

        _boltIcon = new ContentControl
        {
            Width = 11,
            Height = 11,
            Margin = new Thickness(0, 0, 3, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = Brushes.White,
            IsHitTestVisible = false,
            Visibility = Visibility.Collapsed
        };
        _boltIcon.SetResourceReference(StyleProperty, "WidgetGlyph");
        _boltIcon.SetResourceReference(ContentProperty, "IconZap");

        _percentShadow = new TextBlock
        {
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Foreground = new SolidColorBrush(Color.FromArgb(0x58, 0x00, 0x00, 0x00)),
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
            VerticalAlignment = VerticalAlignment.Center,
            IsHitTestVisible = false,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        _percentText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");

        var shadowRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            IsHitTestVisible = false
        };
        shadowRow.Children.Add(_boltShadow);
        shadowRow.Children.Add(_percentShadow);

        var labelRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            IsHitTestVisible = false
        };
        labelRow.Children.Add(_boltIcon);
        labelRow.Children.Add(_percentText);

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
        _surface.Children.Add(_trackLayer);
        _surface.Children.Add(_liquidFill);
        _surface.Children.Add(_sheenClipHost);
        _surface.Children.Add(hoverOverlay);
        _surface.Children.Add(shadowRow);
        _surface.Children.Add(labelRow);
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
        StopChargingAnimation();
    }

    protected override void OnDisposed()
    {
        StopChargingAnimation();
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
            StopChargingAnimation();
            _trackLayer.Visibility = Visibility.Collapsed;
            _liquidFill.Visibility = Visibility.Collapsed;
            _boltIcon.Visibility = Visibility.Collapsed;
            _boltShadow.Visibility = Visibility.Collapsed;
            _percentShadow.Visibility = Visibility.Collapsed;
            _percentText.Text = "—";
            _percentText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            var unavailable = UiText.Get("SystemDataUnavailable");
            _card.ToolTip = $"{UiText.Get("WidgetBattery")}: {unavailable}";
            AutomationProperties.SetName(_card, $"{UiText.Get("WidgetBattery")}: {unavailable}");
            return;
        }

        _trackLayer.Visibility = Visibility.Visible;
        _liquidFill.Visibility = Visibility.Visible;
        _liquidFill.Fill = CreatePaletteBrush(percent);

        var boltVis = charging ? Visibility.Visible : Visibility.Collapsed;
        _boltIcon.Visibility = boltVis;
        _boltShadow.Visibility = boltVis;

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
            && SystemParameters.ClientAreaAnimation;

        if (!shouldAnimate)
        {
            StopChargingAnimation();
            _liquidFill.Clip = BuildStaticFillGeometry(edgeX, height);
            return;
        }

        if (_isAnimatingCharging && !forceAnimationRestart) return;

        StartChargingAnimation(edgeX, height, animateWave: _renderedPercent < 100);
    }

    private static PathGeometry BuildStaticFillGeometry(double edgeX, double height)
    {
        var figure = new PathFigure
        {
            StartPoint = new Point(0, 0),
            IsClosed = true,
            IsFilled = true,
            Segments =
            {
                new LineSegment(new Point(edgeX, 0), isStroked: false),
                new LineSegment(new Point(edgeX, height), isStroked: false),
                new LineSegment(new Point(0, height), isStroked: false)
            }
        };
        var geometry = new PathGeometry { Figures = { figure } };
        geometry.Freeze();
        return geometry;
    }

    /// <summary>
    /// Builds a 2-period periodic sinusoidal meniscus along <paramref name="edgeX"/> so translating Y
    /// from 0 to -<paramref name="height"/> loops seamlessly without reversing direction.
    /// </summary>
    private PathGeometry BuildScrollingWaveGeometry(double edgeX, double height)
    {
        var amp = Math.Min(WaveAmplitude, edgeX * 0.35);
        var half = height * 0.5;
        var figure = new PathFigure
        {
            StartPoint = new Point(0, 0),
            IsClosed = true,
            IsFilled = true,
            Segments =
            {
                new LineSegment(new Point(edgeX, 0), isStroked: false),
                // Period 1: 0 -> height
                new BezierSegment(
                    new Point(edgeX + amp, half * 0.36),
                    new Point(edgeX + amp, half * 0.64),
                    new Point(edgeX, half), isStroked: false),
                new BezierSegment(
                    new Point(edgeX - amp, half + half * 0.36),
                    new Point(edgeX - amp, half + half * 0.64),
                    new Point(edgeX, height), isStroked: false),
                // Period 2: height -> 2 * height (identical to period 1 for seamless tiling)
                new BezierSegment(
                    new Point(edgeX + amp, height + half * 0.36),
                    new Point(edgeX + amp, height + half * 0.64),
                    new Point(edgeX, height + half), isStroked: false),
                new BezierSegment(
                    new Point(edgeX - amp, height + half + half * 0.36),
                    new Point(edgeX - amp, height + half + half * 0.64),
                    new Point(edgeX, height * 2), isStroked: false),
                new LineSegment(new Point(0, height * 2), isStroked: false)
            }
        };
        var geometry = new PathGeometry { Figures = { figure }, Transform = _waveTransform };
        return geometry;
    }

    private void StartChargingAnimation(double edgeX, double height, bool animateWave)
    {
        StopChargingAnimation();
        _isAnimatingCharging = true;

        if (animateWave)
        {
            // Keep _liquidFill stationary so its vertical gradient never drifts; animate only the wave clip geometry.
            var waveClip = BuildScrollingWaveGeometry(edgeX, height);
            _liquidFill.Clip = waveClip;
            _sheenClipHost.Clip = waveClip;
            var waveAnim = new DoubleAnimation(0, -height, TimeSpan.FromMilliseconds(1600))
            {
                RepeatBehavior = RepeatBehavior.Forever
            };
            _waveTransform.BeginAnimation(TranslateTransform.YProperty, waveAnim);
        }
        else
        {
            var staticClip = BuildStaticFillGeometry(edgeX, height);
            _liquidFill.Clip = staticClip;
            _sheenClipHost.Clip = staticClip;
        }

        // Slanted soft charging sheen with a calm pause between sweeps.
        _chargeSheen.Visibility = Visibility.Visible;
        var startX = -SheenWidth - 28;
        var endX = edgeX + 28;
        var sheenAnim = new DoubleAnimationUsingKeyFrames
        {
            Duration = TimeSpan.FromMilliseconds(2800),
            RepeatBehavior = RepeatBehavior.Forever
        };
        sheenAnim.KeyFrames.Add(new DiscreteDoubleKeyFrame(startX, KeyTime.FromTimeSpan(TimeSpan.Zero)));
        sheenAnim.KeyFrames.Add(new EasingDoubleKeyFrame(
            endX,
            KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(1400)),
            new CubicEase { EasingMode = EasingMode.EaseInOut }));
        sheenAnim.KeyFrames.Add(new DiscreteDoubleKeyFrame(endX, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(2800))));
        _sheenTransform.BeginAnimation(TranslateTransform.XProperty, sheenAnim);
    }

    private void StopChargingAnimation()
    {
        if (!_isAnimatingCharging) return;
        _isAnimatingCharging = false;
        _waveTransform.BeginAnimation(TranslateTransform.YProperty, null);
        _waveTransform.Y = 0;
        _sheenTransform.BeginAnimation(TranslateTransform.XProperty, null);
        _sheenTransform.X = -SheenWidth;
        _chargeSheen.Visibility = Visibility.Collapsed;
    }
}
