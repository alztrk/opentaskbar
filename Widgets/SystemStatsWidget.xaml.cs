using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// V19 System Stats widget: compact horizontal CPU / RAM / GPU metrics on the dock,
/// with a dedicated metrics flyout showing progress bars, temperatures, and Task Manager shortcut.
/// </summary>
public partial class SystemStatsWidget : DockWidget
{
    private const double TemperatureScaleCelsius = 100;
    private const double ValueEpsilon = 0.005;
    private const long FastSamplePeriodMs = 1800;
    private const long SensorSamplePeriodMs = 3800;
    private const int SensorHideMisses = 5;
    private const int SensorShowSamples = 2;

    private struct MetricState
    {
        public bool Initialized;
        public int Shown;
        public double Value;
        public AppLanguage Language;
    }

    private readonly SystemStats _stats = new();
    private readonly object _sensorGate = new();
    private readonly DockFlyout _flyout = new();
    private SystemSensors? _sensors;
    private int _sensorGeneration;
    private int _sensorSampling;
    private long _lastFastSampleTickMs;
    private long _lastSensorSampleTickMs;

    private MetricState _cpuState;
    private MetricState _memoryState;
    private MetricState _gpuState;
    private MetricState _cpuTempState;
    private MetricState _gpuTempState;
    private bool _hasGpu;
    private bool _hasCpuTemp;
    private bool _hasGpuTemp;
    private SystemMeters _configuredMeters;
    private readonly Dictionary<FrameworkElement, int> _missingSensorSamples = [];
    private readonly Dictionary<FrameworkElement, int> _validSensorSamples = [];

    private Border? _flyoutHost;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 30;
    public override double CompactWidthEstimate => 112;
    public override double FullWidthEstimate
    {
        get
        {
            var meters = Settings.StatsMeters;
            var count = 0;
            if (meters.HasFlag(SystemMeters.Cpu)) count++;
            if (meters.HasFlag(SystemMeters.Memory)) count++;
            if (meters.HasFlag(SystemMeters.Gpu) && _hasGpu) count++;
            if (meters.HasFlag(SystemMeters.CpuTemperature) && _hasCpuTemp) count++;
            if (meters.HasFlag(SystemMeters.GpuTemperature) && _hasGpuTemp) count++;
            return Math.Max(96, 36 + count * 48);
        }
    }

    public SystemStatsWidget()
    {
        InitializeComponent();
        _flyout.Closed += (_, _) => _flyoutHost = null;
    }

    protected override void OnCompactChanged(bool compact) => SyncMeterVisibility();
    protected override void OnHoverExpandedChanged(bool expanded) => SyncMeterVisibility();

    public override UIElement? BuildHoverFlyoutContent() => BuildMetricsFlyout();

    private void StatsCardButton_Click(object sender, RoutedEventArgs e)
    {
        _flyout.Toggle(StatsCardButton, () =>
        {
            var host = new Border { Width = 292 };
            _flyoutHost = host;
            host.Child = BuildMetricsFlyout();
            return host;
        }, animate: true, focusable: false);
    }

    private void RefreshOpenFlyout()
    {
        if (_flyout.IsOpen && _flyoutHost is not null)
        {
            _flyoutHost.Child = BuildMetricsFlyout();
        }
    }

    internal UIElement BuildMetricsFlyout()
    {
        var root = new StackPanel();
        root.Children.Add(DockFlyout.Heading(UiText.Get("WidgetSystemStats")));

        var meters = Settings.StatsMeters;
        if (meters.HasFlag(SystemMeters.Cpu))
            AddMetricRow(root, UiText.Get("Cpu"), _cpuState.Initialized ? $"{_cpuState.Shown}%" : "—", _cpuState.Value);
        if (meters.HasFlag(SystemMeters.Memory))
            AddMetricRow(root, UiText.Get("Memory"), _memoryState.Initialized ? $"{_memoryState.Shown}%" : "—", _memoryState.Value);
        if (meters.HasFlag(SystemMeters.Gpu) && _hasGpu)
            AddMetricRow(root, UiText.Get("Gpu"), $"{_gpuState.Shown}%", _gpuState.Value);
        if (meters.HasFlag(SystemMeters.CpuTemperature) && _hasCpuTemp)
            AddPlainRow(root, UiText.Get("CpuTemperature"), $"{_cpuTempState.Shown} °C");
        if (meters.HasFlag(SystemMeters.GpuTemperature) && _hasGpuTemp)
            AddPlainRow(root, UiText.Get("GpuTemperature"), $"{_gpuTempState.Shown} °C");

        var divider = new Border { Height = 1, Margin = new Thickness(0, 10, 0, 8) };
        divider.SetResourceReference(Border.BackgroundProperty, "StrokeSubtle");
        root.Children.Add(divider);

        var tmBtn = new Button
        {
            Content = UiText.Get("TaskManager"),
            HorizontalAlignment = HorizontalAlignment.Stretch
        };
        tmBtn.SetResourceReference(StyleProperty, "WidgetFlyoutActionButton");
        tmBtn.Click += (_, _) =>
        {
            _flyout.Close();
            Report(ShellShortcuts.OpenTaskManager());
        };
        root.Children.Add(tmBtn);

        return root;
    }

    private static void AddMetricRow(Panel parent, string label, string valueText, double normalized)
    {
        var row = new DockPanel { Margin = new Thickness(0, 4, 0, 4) };
        var val = new TextBlock
        {
            Text = valueText,
            FontSize = 12.5,
            FontWeight = FontWeights.SemiBold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        val.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        DockPanel.SetDock(val, Dock.Right);
        row.Children.Add(val);

        var lbl = new TextBlock { Text = label, FontSize = 12 };
        lbl.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        row.Children.Add(lbl);
        parent.Children.Add(row);

        var track = new Border { Height = 6, CornerRadius = new CornerRadius(3), Margin = new Thickness(0, 0, 0, 8), ClipToBounds = true };
        track.SetResourceReference(Border.BackgroundProperty, "WidgetTrack");

        var pct = Math.Clamp(normalized, 0, 1);
        var fillGrid = new Grid();
        fillGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(pct, GridUnitType.Star) });
        fillGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1 - pct, GridUnitType.Star) });

        var fill = new Border { CornerRadius = new CornerRadius(3) };
        fill.SetResourceReference(Border.BackgroundProperty,
            pct >= 0.9 ? "WidgetFocusAccent" : pct >= 0.75 ? "WidgetWarning" : "WidgetBlue");
        Grid.SetColumn(fill, 0);
        fillGrid.Children.Add(fill);
        track.Child = fillGrid;
        parent.Children.Add(track);
    }

    private static void AddPlainRow(Panel parent, string label, string valueText)
    {
        var row = new DockPanel { Margin = new Thickness(0, 4, 0, 4) };
        var val = new TextBlock
        {
            Text = valueText,
            FontSize = 12.5,
            FontWeight = FontWeights.SemiBold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        val.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        DockPanel.SetDock(val, Dock.Right);
        row.Children.Add(val);

        var lbl = new TextBlock { Text = label, FontSize = 12 };
        lbl.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        row.Children.Add(lbl);
        parent.Children.Add(row);
    }

    protected override void OnActivated()
    {
        _stats.SampleCpu();
        Interlocked.Increment(ref _sensorGeneration);
        ApplyMeterSelection();
        SampleFastCounters(sampleCpu: false);
        var nowMs = Environment.TickCount64;
        _lastFastSampleTickMs = nowMs - 900;
        _lastSensorSampleTickMs = nowMs - 2900;
        PrimeSensorBaselineIfNeeded();
    }

    protected override void OnDeactivated()
    {
        _flyout.Close();
        var generation = Interlocked.Increment(ref _sensorGeneration);
        _ = Task.Run(() =>
        {
            lock (_sensorGate)
            {
                if (generation != Volatile.Read(ref _sensorGeneration)) return;
                _sensors?.Dispose();
                _sensors = null;
            }
        });
        _lastFastSampleTickMs = 0;
        _lastSensorSampleTickMs = 0;
    }

    protected override void OnDisposed()
    {
        _flyout.Close();
        base.OnDisposed();
    }

    protected override void OnSettingsChanged()
    {
        Interlocked.Increment(ref _sensorGeneration);
        ApplyMeterSelection();
        if (IsActive)
        {
            _lastFastSampleTickMs = 0;
            _lastSensorSampleTickMs = 0;
            OnTick(DateTime.Now);
        }
    }

    protected override void OnTick(DateTime now)
    {
        var tickMs = Environment.TickCount64;
        if (_lastFastSampleTickMs == 0 || tickMs - _lastFastSampleTickMs >= FastSamplePeriodMs)
        {
            _lastFastSampleTickMs = tickMs;
            SampleFastCounters(sampleCpu: true);
        }

        if (_lastSensorSampleTickMs == 0 || tickMs - _lastSensorSampleTickMs >= SensorSamplePeriodMs)
        {
            _lastSensorSampleTickMs = tickMs;
            QueueSensorSample(Settings.StatsMeters);
        }
    }

    private void SampleFastCounters(bool sampleCpu)
    {
        var meters = Settings.StatsMeters;
        if (sampleCpu && meters.HasFlag(SystemMeters.Cpu) && _stats.SampleCpu() is { } cpu)
            SetUsage(CpuValueText, ref _cpuState, cpu);
        if (meters.HasFlag(SystemMeters.Memory) && SystemStats.SampleMemory() is { } memory)
            SetUsage(RamValueText, ref _memoryState, memory);
        UpdateTooltip();
        RefreshOpenFlyout();
    }

    private void PrimeSensorBaselineIfNeeded()
    {
        var meters = Settings.StatsMeters;
        if ((meters & (SystemMeters.Gpu | SystemMeters.CpuTemperature | SystemMeters.GpuTemperature)) == 0) return;
        if (Interlocked.CompareExchange(ref _sensorSampling, 1, 0) != 0) return;
        var generation = Volatile.Read(ref _sensorGeneration);
        _ = Task.Run(async () =>
        {
            double? gpu = null;
            double? cpuTemp = null;
            double? gpuTemp = null;
            try
            {
                lock (_sensorGate)
                {
                    if (generation != Volatile.Read(ref _sensorGeneration)) return;
                    _sensors ??= new SystemSensors(meters);
                    _sensors.Configure(meters);
                    if (meters.HasFlag(SystemMeters.Gpu)) _sensors.SampleGpuUsage();
                }

                await Task.Delay(350).ConfigureAwait(false);

                lock (_sensorGate)
                {
                    if (generation != Volatile.Read(ref _sensorGeneration)) return;
                    if (meters.HasFlag(SystemMeters.Gpu)) gpu = _sensors?.SampleGpuUsage();
                    if (meters.HasFlag(SystemMeters.CpuTemperature)) cpuTemp = _sensors?.SampleCpuTemperature();
                    if (meters.HasFlag(SystemMeters.GpuTemperature)) gpuTemp = _sensors?.SampleGpuTemperature();
                }
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Sensor baseline/initial sample failed: {ex.Message}");
                return;
            }
            finally
            {
                Interlocked.Exchange(ref _sensorSampling, 0);
            }

            _ = Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
            {
                if (!IsActive || generation != Volatile.Read(ref _sensorGeneration)) return;
                ApplySensorBatch(gpu, cpuTemp, gpuTemp);
            });
        });
    }

    private void QueueSensorSample(SystemMeters meters)
    {
        if ((meters & (SystemMeters.Gpu | SystemMeters.CpuTemperature | SystemMeters.GpuTemperature)) == 0) return;
        if (Interlocked.CompareExchange(ref _sensorSampling, 1, 0) != 0) return;
        var generation = Volatile.Read(ref _sensorGeneration);

        _ = Task.Run(() =>
        {
            double? gpu = null;
            double? cpuTemp = null;
            double? gpuTemp = null;
            try
            {
                lock (_sensorGate)
                {
                    if (generation != Volatile.Read(ref _sensorGeneration)) return;
                    _sensors ??= new SystemSensors(meters);
                    _sensors.Configure(meters);
                    if (meters.HasFlag(SystemMeters.Gpu)) gpu = _sensors.SampleGpuUsage();
                    if (meters.HasFlag(SystemMeters.CpuTemperature)) cpuTemp = _sensors.SampleCpuTemperature();
                    if (meters.HasFlag(SystemMeters.GpuTemperature)) gpuTemp = _sensors.SampleGpuTemperature();
                }
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Sensor sampling failed: {ex.Message}");
                return;
            }
            finally
            {
                Interlocked.Exchange(ref _sensorSampling, 0);
            }

            _ = Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
            {
                if (!IsActive || generation != Volatile.Read(ref _sensorGeneration)) return;
                ApplySensorBatch(gpu, cpuTemp, gpuTemp);
            });
        });
    }

    private void ApplySensorBatch(double? gpu, double? cpuTemp, double? gpuTemp)
    {
        var currentMeters = Settings.StatsMeters;
        UpdateSensor(GpuButton, currentMeters.HasFlag(SystemMeters.Gpu) ? gpu : null,
            usage => SetUsage(GpuValueText, ref _gpuState, usage));
        UpdateSensor(CpuTemperatureButton, currentMeters.HasFlag(SystemMeters.CpuTemperature) ? cpuTemp : null,
            celsius => SetTemperature(CpuTempValueText, ref _cpuTempState, celsius));
        UpdateSensor(GpuTemperatureButton, currentMeters.HasFlag(SystemMeters.GpuTemperature) ? gpuTemp : null,
            celsius => SetTemperature(GpuTempValueText, ref _gpuTempState, celsius));
        UpdateTooltip();
        RefreshOpenFlyout();
    }

    private void ApplyMeterSelection()
    {
        var meters = Settings.StatsMeters;
        if (_configuredMeters != meters)
        {
            _configuredMeters = meters;
            _hasGpu = false;
            _hasCpuTemp = false;
            _hasGpuTemp = false;
            _missingSensorSamples.Clear();
            _validSensorSamples.Clear();
        }
        SyncMeterVisibility();
    }

    private void SyncMeterVisibility()
    {
        var compact = IsCompact && !IsHoverExpanded;
        var meters = Settings.StatsMeters;
        var changed = false;

        var showCpu = meters.HasFlag(SystemMeters.Cpu);
        var showRam = meters.HasFlag(SystemMeters.Memory);
        var showGpu = !compact && meters.HasFlag(SystemMeters.Gpu) && _hasGpu;
        var showCpuTemp = !compact && meters.HasFlag(SystemMeters.CpuTemperature) && _hasCpuTemp;
        var showGpuTemp = !compact && meters.HasFlag(SystemMeters.GpuTemperature) && _hasGpuTemp;

        CpuLabel.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        RamLabel.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        GpuLabel.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;

        Set(CpuButton, showCpu);
        Set(RamDivider, showCpu && showRam);
        Set(MemoryButton, showRam);
        Set(GpuDivider, (showCpu || showRam) && showGpu);
        Set(GpuButton, showGpu);
        Set(CpuTempDivider, (showCpu || showRam || showGpu) && showCpuTemp);
        Set(CpuTemperatureButton, showCpuTemp);
        Set(GpuTempDivider, (showCpu || showRam || showGpu || showCpuTemp) && showGpuTemp);
        Set(GpuTemperatureButton, showGpuTemp);

        if (changed) RaiseContentWidthChanged();

        void Set(FrameworkElement element, bool visible)
        {
            var target = visible ? Visibility.Visible : Visibility.Collapsed;
            if (element.Visibility == target) return;
            element.Visibility = target;
            changed = true;
        }
    }

    private void UpdateSensor(FrameworkElement button, double? reading, Action<double> render)
    {
        if (reading is null)
        {
            var misses = Math.Min(_missingSensorSamples.GetValueOrDefault(button) + 1, SensorHideMisses);
            _missingSensorSamples[button] = misses;
            _validSensorSamples.Remove(button);
            if (misses >= SensorHideMisses) SetAvailability(button, false);
            return;
        }

        _missingSensorSamples.Remove(button);
        var valid = Math.Min(_validSensorSamples.GetValueOrDefault(button) + 1, SensorShowSamples);
        _validSensorSamples[button] = valid;
        render(reading.Value);
        if (valid >= SensorShowSamples) SetAvailability(button, true);
    }

    /// <summary>Test seam: feeds a reading through the same availability logic as the native sensors.</summary>
    internal void ApplySensorReading(FrameworkElement button, double? reading, Action<double>? render = null) =>
        UpdateSensor(button, reading, render ?? (_ => { }));

    private void SetAvailability(FrameworkElement button, bool available)
    {
        if (button == GpuButton)
        {
            if (_hasGpu == available) return;
            _hasGpu = available;
        }
        else if (button == CpuTemperatureButton)
        {
            if (_hasCpuTemp == available) return;
            _hasCpuTemp = available;
        }
        else if (button == GpuTemperatureButton)
        {
            if (_hasGpuTemp == available) return;
            _hasGpuTemp = available;
        }
        else return;

        SyncMeterVisibility();
    }

    private void SetUsage(TextBlock target, ref MetricState state, double value)
    {
        var percent = (int)Math.Round(value * 100);
        var lang = Settings.Language;
        if (state.Initialized && state.Shown == percent && state.Language == lang && Math.Abs(value - state.Value) < ValueEpsilon)
            return;

        state.Value = value;
        state.Shown = percent;
        state.Language = lang;
        state.Initialized = true;
        target.Text = $"{percent.ToString(CultureInfo.InvariantCulture)}%";
    }

    private void SetTemperature(TextBlock target, ref MetricState state, double celsius)
    {
        var rounded = (int)Math.Round(celsius);
        var normalized = celsius / TemperatureScaleCelsius;
        var lang = Settings.Language;
        if (state.Initialized && state.Shown == rounded && state.Language == lang && Math.Abs(normalized - state.Value) < ValueEpsilon)
            return;

        state.Value = normalized;
        state.Shown = rounded;
        state.Language = lang;
        state.Initialized = true;
        target.Text = $"{rounded.ToString(CultureInfo.InvariantCulture)}°";
    }

    private void UpdateTooltip()
    {
        var parts = new List<string>();
        if (_cpuState.Initialized) parts.Add($"CPU {_cpuState.Shown}%");
        if (_memoryState.Initialized) parts.Add($"RAM {_memoryState.Shown}%");
        if (_hasGpu && _gpuState.Initialized) parts.Add($"GPU {_gpuState.Shown}%");
        if (_hasCpuTemp && _cpuTempState.Initialized) parts.Add($"CPU {_cpuTempState.Shown}°C");
        if (_hasGpuTemp && _gpuTempState.Initialized) parts.Add($"GPU {_gpuTempState.Shown}°C");
        StatsCardButton.ToolTip = parts.Count > 0 ? string.Join(" · ", parts) : UiText.Get("WidgetSystemStats");
    }
}
