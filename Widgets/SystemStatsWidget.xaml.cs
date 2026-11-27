using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Live system metrics and their history, with process details sampled while the panel is open.
/// </summary>
public partial class SystemStatsWidget : DockWidget
{
    private const double TemperatureScaleCelsius = 100.0;
    private const double WarningThreshold = 0.75;
    private const double CriticalThreshold = 0.90;
    private const double ThermalWarningCelsius = 70.0;
    private const double ThermalCriticalCelsius = 85.0;
    private const double ValueEpsilon = 0.005;
    private const long FastSamplePeriodMs = 900;
    private const long SensorSamplePeriodMs = 1900;
    private const long ProcessSamplePeriodMs = 2000;
    private const int SensorHideMisses = 5;
    private const int SensorShowSamples = 2;
    private const int SparklineCapacity = 45;
    private const int TopProcessCount = 5;

    private struct MetricState
    {
        public bool Initialized;
        public int Shown;
        public double Value;
        public string? TextBrushKey;
        public AppLanguage Language;
    }

    private sealed class TelemetryFlyoutView
    {
        public required TextBlock UptimeText { get; init; }
        public required TextBlock CpuValueText { get; init; }
        public required TelemetrySparkline CpuSparkline { get; init; }

        public required TextBlock RamValueText { get; init; }
        public required TextBlock RamPairText { get; init; }
        public required TelemetrySparkline RamSparkline { get; init; }

        public required StackPanel GpuSection { get; init; }
        public required TextBlock GpuValueText { get; init; }
        public required TextBlock GpuTempText { get; init; }
        public required TelemetrySparkline GpuSparkline { get; init; }

        public required StackPanel ProcessesList { get; init; }
    }

    private sealed class HoverFlyoutView
    {
        public required Border Container { get; init; }
        public required TextBlock UptimeText { get; init; }
        public required TextBlock CpuValueText { get; init; }
        public required TextBlock RamValueText { get; init; }
        public required TextBlock RamDetailText { get; init; }
        public required DockPanel GpuRow { get; init; }
        public required TextBlock GpuValueText { get; init; }
    }

    private readonly SystemStats _stats = new();
    private readonly DockFlyout _flyout = new();
    private readonly object _sensorGate = new();
    private readonly MetricRingBuffer _cpuHistory = new(SparklineCapacity);
    private readonly MetricRingBuffer _memoryHistory = new(SparklineCapacity);
    private readonly MetricRingBuffer _gpuHistory = new(SparklineCapacity);
    private readonly Dictionary<FrameworkElement, int> _missingSensorSamples = [];
    private readonly Dictionary<FrameworkElement, int> _validSensorSamples = [];

    private SystemSensors? _sensors;
    private int _sensorGeneration;
    private int _sensorSampling;
    private long _lastFastSampleTickMs;
    private long _lastSensorSampleTickMs;

    private CancellationTokenSource? _processSampleCts;
    private int _processGeneration;
    private int _processSampling;
    private long _lastProcessSampleTickMs;
    private IReadOnlyList<ProcessMemoryEntry> _lastTopProcesses = [];

    private MetricState _cpuState;
    private MetricState _memoryState;
    private MetricState _gpuState;
    private MetricState _gpuTempState;
    private MemorySnapshot? _lastMemorySnapshot;
    private bool _hasGpu;
    private bool _hasGpuTemp;
    private SystemMeters _configuredMeters;
    private bool? _lastAppliedCompact;
    private bool? _lastAppliedVertical;

    private TelemetryFlyoutView? _clickFlyout;
    private HoverFlyoutView? _hoverFlyout;

    internal DockFlyout ClickFlyout => _flyout;
    internal Func<int, CancellationToken, IReadOnlyList<ProcessMemoryEntry>> TopProcessesReader { get; set; } = SystemStats.SampleTopProcesses;
    internal Action OpenTaskManagerAction { get; set; }
    internal Action OpenResourceMonitorAction { get; set; }

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 30;
    public override double CompactWidthEstimate => 84;
    public override double FullWidthEstimate
    {
        get
        {
            var meters = Settings.StatsMeters;
            var count = 0;
            if (meters.HasFlag(SystemMeters.Cpu)) count++;
            if (meters.HasFlag(SystemMeters.Memory)) count++;
            if (meters.HasFlag(SystemMeters.Gpu) && _hasGpu) count++;
            if (meters.HasFlag(SystemMeters.GpuTemperature) && _hasGpuTemp) count++;
            return 16 + count * 36 + Math.Max(0, count - 1) * 7;
        }
    }

    public SystemStatsWidget()
    {
        InitializeComponent();
        OpenTaskManagerAction = () => Report(ShellShortcuts.OpenTaskManager());
        OpenResourceMonitorAction = () => Report(ShellShortcuts.OpenResourceMonitor());
        _flyout.Closed += Flyout_Closed;
    }

    protected override void OnCompactChanged(bool compact) => SyncMeterVisibility();
    protected override void OnHoverExpandedChanged(bool expanded) => SyncMeterVisibility();

    public override UIElement? BuildHoverFlyoutContent()
    {
        if (_flyout.IsOpen) return null;
        var view = CreateHoverFlyoutView();
        _hoverFlyout = view;
        view.Container.Unloaded += (_, _) =>
        {
            if (ReferenceEquals(_hoverFlyout, view)) _hoverFlyout = null;
        };
        RefreshHoverFlyout(view);
        return view.Container;
    }

    public override UIElement? BuildPreviewFlyoutContent() => BuildTelemetryFlyout(isPreview: true);

    protected override void OnActivated()
    {
        _stats.SampleCpu();
        Interlocked.Increment(ref _sensorGeneration);
        ApplyMeterSelection();
        SampleFastCounters(sampleCpu: false);
        var nowMs = Environment.TickCount64;
        _lastFastSampleTickMs = nowMs;
        _lastSensorSampleTickMs = nowMs - 1000;
        PrimeSensorBaselineIfNeeded();
    }

    protected override void OnDeactivated()
    {
        _flyout.Close(restoreFocus: false);
        CancelProcessSampling();
        _clickFlyout = null;
        _hoverFlyout = null;

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
        _lastProcessSampleTickMs = 0;
    }

    protected override void OnDisposed() => _flyout.Closed -= Flyout_Closed;

    protected override void OnSettingsChanged()
    {
        ApplyMeterSelection();
        if (IsActive)
        {
            Interlocked.Increment(ref _sensorGeneration);
            _lastFastSampleTickMs = 0;
            _lastSensorSampleTickMs = 0;
            OnTick(DateTime.Now);
        }
    }

    protected override void OnLanguageChanged()
    {
        UpdateTooltip();
        if (_flyout.IsOpen)
        {
            _flyout.Close(restoreFocus: false);
        }
        if (_hoverFlyout is not null && IsHoverFlyoutOpen)
        {
            RefreshHoverFlyout(_hoverFlyout);
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

        if (_flyout.IsOpen && _clickFlyout is not null)
        {
            RefreshTelemetryFlyout(_clickFlyout);
            if (_lastProcessSampleTickMs == 0 || tickMs - _lastProcessSampleTickMs >= ProcessSamplePeriodMs)
            {
                _lastProcessSampleTickMs = tickMs;
                if (_processSampleCts is { IsCancellationRequested: false } cts)
                {
                    QueueProcessSample(cts.Token);
                }
            }
        }
        else if (IsHoverFlyoutOpen && _hoverFlyout is not null)
        {
            RefreshHoverFlyout(_hoverFlyout);
        }
    }

    private void StatsCardButton_Click(object sender, RoutedEventArgs e)
    {
        _flyout.Toggle(StatsCardButton, () => BuildTelemetryFlyout(isPreview: false), animate: true, focusable: false);
    }

    private void Flyout_Closed(object? sender, EventArgs e)
    {
        CancelProcessSampling();
        _clickFlyout = null;
        _lastProcessSampleTickMs = 0;
    }

    private void SampleFastCounters(bool sampleCpu)
    {
        if (sampleCpu && _stats.SampleCpu() is { } cpu)
        {
            _cpuHistory.Push(cpu);
            SetUsage(CpuValueText, ref _cpuState, cpu);
        }

        if (SystemStats.SampleMemorySnapshot() is { } memorySnapshot)
        {
            _lastMemorySnapshot = memorySnapshot;
            _memoryHistory.Push(memorySnapshot.UsageRatio);
            SetUsage(RamValueText, ref _memoryState, memorySnapshot.UsageRatio);
        }

        UpdateTooltip();
    }

    private void PrimeSensorBaselineIfNeeded()
    {
        var meters = Settings.StatsMeters;
        if ((meters & (SystemMeters.Gpu | SystemMeters.GpuTemperature)) == 0) return;
        if (Interlocked.CompareExchange(ref _sensorSampling, 1, 0) != 0) return;
        var generation = Volatile.Read(ref _sensorGeneration);
        _ = Task.Run(async () =>
        {
            double? gpu = null;
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
                ApplySensorBatch(gpu, gpuTemp);
            });
        });
    }

    private void QueueSensorSample(SystemMeters meters)
    {
        if ((meters & (SystemMeters.Gpu | SystemMeters.GpuTemperature)) == 0) return;
        if (Interlocked.CompareExchange(ref _sensorSampling, 1, 0) != 0) return;
        var generation = Volatile.Read(ref _sensorGeneration);

        _ = Task.Run(() =>
        {
            double? gpu = null;
            double? gpuTemp = null;
            try
            {
                lock (_sensorGate)
                {
                    if (generation != Volatile.Read(ref _sensorGeneration)) return;
                    _sensors ??= new SystemSensors(meters);
                    _sensors.Configure(meters);
                    if (meters.HasFlag(SystemMeters.Gpu)) gpu = _sensors.SampleGpuUsage();
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
                ApplySensorBatch(gpu, gpuTemp);
            });
        });
    }

    private void ApplySensorBatch(double? gpu, double? gpuTemp)
    {
        var currentMeters = Settings.StatsMeters;
        UpdateSensor(GpuButton, currentMeters.HasFlag(SystemMeters.Gpu) ? gpu : null,
            usage =>
            {
                _gpuHistory.Push(usage);
                SetUsage(GpuValueText, ref _gpuState, usage);
            });
        UpdateSensor(GpuTemperatureButton, currentMeters.HasFlag(SystemMeters.GpuTemperature) ? gpuTemp : null,
            celsius => SetTemperature(GpuTempValueText, ref _gpuTempState, celsius));

        UpdateTooltip();

        if (_flyout.IsOpen && _clickFlyout is not null)
            RefreshTelemetryFlyout(_clickFlyout);
        else if (IsHoverFlyoutOpen && _hoverFlyout is not null)
            RefreshHoverFlyout(_hoverFlyout);
    }

    private void ApplyMeterSelection()
    {
        var meters = Settings.StatsMeters;
        if (_configuredMeters != meters)
        {
            _configuredMeters = meters;
            _hasGpu = false;
            _hasGpuTemp = false;
            _missingSensorSamples.Clear();
            _validSensorSamples.Clear();
            if (!meters.HasFlag(SystemMeters.Gpu)) _gpuHistory.Clear();
        }
        SyncMeterVisibility();
    }

    private void SyncMeterVisibility()
    {
        var compact = IsCompact && !IsHoverExpanded;
        var vertical = Settings.Edge != DockEdge.Bottom;
        var meters = Settings.StatsMeters;
        var changed = _lastAppliedCompact != compact || _lastAppliedVertical != vertical;
        _lastAppliedCompact = compact;
        _lastAppliedVertical = vertical;

        var showCpu = meters.HasFlag(SystemMeters.Cpu);
        var showRam = meters.HasFlag(SystemMeters.Memory);
        var showGpu = !compact && meters.HasFlag(SystemMeters.Gpu) && _hasGpu;
        var showGpuTemp = !compact && meters.HasFlag(SystemMeters.GpuTemperature) && _hasGpuTemp;

        MetricsPanel.Orientation = vertical ? Orientation.Vertical : Orientation.Horizontal;
        StatsCardButton.Padding = vertical
            ? (compact ? new Thickness(6, 5, 6, 5) : new Thickness(8, 6, 8, 6))
            : (compact ? new Thickness(6, 0, 6, 0) : new Thickness(8, 0, 8, 0));
        var dividerMargin = vertical
            ? new Thickness(0, compact ? 4 : 5, 0, compact ? 4 : 5)
            : new Thickness(3, 0, 3, 0);
        foreach (var divider in new[] { RamDivider, GpuDivider, GpuTempDivider })
        {
            divider.Width = vertical ? 22 : 1;
            divider.Height = vertical ? 1 : (compact ? 16 : 20);
            divider.Margin = dividerMargin;
        }

        var metricMinWidth = compact ? 32.0 : 36.0;
        CpuButton.MinWidth = metricMinWidth;
        MemoryButton.MinWidth = metricMinWidth;
        GpuButton.MinWidth = metricMinWidth;
        GpuTemperatureButton.MinWidth = compact ? 32.0 : 36.0;

        Set(CpuLabel, !compact);
        Set(RamLabel, !compact);
        Set(GpuLabel, !compact);
        Set(GpuTempLabel, !compact);

        Set(CpuButton, showCpu);
        Set(RamDivider, showCpu && showRam);
        Set(MemoryButton, showRam);
        Set(GpuDivider, (showCpu || showRam) && showGpu);
        Set(GpuButton, showGpu);
        Set(GpuTempDivider, (showCpu || showRam || showGpu) && showGpuTemp);
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
        else if (button == GpuTemperatureButton)
        {
            if (_hasGpuTemp == available) return;
            _hasGpuTemp = available;
        }
        else return;

        SyncMeterVisibility();
    }

    private void SetUsage(
        TextBlock target,
        ref MetricState state,
        double value)
    {
        var clamped = double.IsFinite(value) ? Math.Clamp(value, 0, 1) : 0;
        var percent = (int)Math.Round(clamped * 100);
        var lang = Settings.Language;
        var textKey = LoadTextBrushKey(clamped);

        if (state.Initialized
            && state.Shown == percent
            && state.Language == lang
            && state.TextBrushKey == textKey
            && Math.Abs(clamped - state.Value) < ValueEpsilon)
            return;

        if (state.TextBrushKey != textKey)
            target.SetResourceReference(TextBlock.ForegroundProperty, textKey);

        state.Value = clamped;
        state.Shown = percent;
        state.Language = lang;
        state.TextBrushKey = textKey;
        state.Initialized = true;

        target.Text = $"{percent.ToString(CultureInfo.InvariantCulture)}%";
    }

    private void SetTemperature(
        TextBlock target,
        ref MetricState state,
        double celsius)
    {
        var rounded = (int)Math.Round(celsius);
        var normalized = celsius / TemperatureScaleCelsius;
        var lang = Settings.Language;
        var textKey = ThermalTextBrushKey(celsius);

        if (state.Initialized
            && state.Shown == rounded
            && state.Language == lang
            && state.TextBrushKey == textKey
            && Math.Abs(normalized - state.Value) < ValueEpsilon)
            return;

        if (state.TextBrushKey != textKey)
            target.SetResourceReference(TextBlock.ForegroundProperty, textKey);

        state.Value = normalized;
        state.Shown = rounded;
        state.Language = lang;
        state.TextBrushKey = textKey;
        state.Initialized = true;

        target.Text = $"{rounded.ToString(CultureInfo.InvariantCulture)}°";
    }

    private void UpdateTooltip()
    {
        var parts = new List<string>(4);
        if (_cpuState.Initialized) parts.Add($"CPU {_cpuState.Shown}%");
        if (_memoryState.Initialized) parts.Add($"RAM {_memoryState.Shown}%");
        if (_hasGpu && _gpuState.Initialized) parts.Add($"GPU {_gpuState.Shown}%");
        if (_hasGpuTemp && _gpuTempState.Initialized) parts.Add($"GPU {_gpuTempState.Shown}°C");
        var summary = parts.Count > 0 ? string.Join(" · ", parts) : UiText.Get("WidgetSystemStats");
        StatsCardButton.ToolTip = summary;
        AutomationProperties.SetName(StatsCardButton, $"{UiText.Get("WidgetSystemStats")}: {summary}");
    }

    internal UIElement BuildTelemetryFlyout(bool isPreview = false)
    {
        var root = new StackPanel();
        var container = new Border { Width = 296, Child = root };

        var uptimeText = CreateValueText();
        uptimeText.FontSize = 11;
        uptimeText.FontWeight = FontWeights.Normal;
        uptimeText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        uptimeText.ToolTip = UiText.Get("StatsUptime");
        root.Children.Add(DockFlyout.Header(UiText.Get("WidgetSystemStats"), uptimeText));

        var cpuValue = CreateValueText();
        var cpuSparkline = CreateSparkline("WidgetBlue", 14);
        root.Children.Add(CreateMetricSection(UiText.Get("Cpu"), cpuValue, cpuSparkline));

        var ramValue = CreateValueText();
        var ramPair = CreateValueText();
        ramPair.FontSize = 11;
        ramPair.FontWeight = FontWeights.Normal;
        ramPair.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        var ramSparkline = CreateSparkline("WidgetAccent", 14);
        root.Children.Add(CreateMetricSection(UiText.Get("Memory"), ramValue, ramSparkline, ramPair));

        var gpuValue = CreateValueText();
        var gpuTemp = CreateValueText();
        gpuTemp.ToolTip = UiText.Get("GpuTemperature");
        var gpuSparkline = CreateSparkline("AccentBrush", 14);
        var gpuSection = CreateMetricSection(UiText.Get("Gpu"), gpuValue, gpuSparkline, gpuTemp);
        root.Children.Add(gpuSection);

        root.Children.Add(DockFlyout.Divider(6));

        var processColumn = CreateValueText();
        processColumn.Text = UiText.Get("Memory");
        processColumn.FontSize = 11;
        processColumn.FontWeight = FontWeights.Normal;
        processColumn.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        var processHeader = CreateMetricLine(UiText.Get("StatsTopProcesses"), processColumn);
        processHeader.Margin = new Thickness(0, 0, 0, 4);
        root.Children.Add(processHeader);

        var processesList = new StackPanel();
        root.Children.Add(processesList);

        var taskManagerLabel = UiText.Get("TaskManager");
        var taskManager = new Button { Content = taskManagerLabel };
        taskManager.SetResourceReference(StyleProperty, "WidgetFlyoutActionButton");
        AutomationProperties.SetName(taskManager, taskManagerLabel);
        taskManager.Click += (_, _) =>
        {
            _flyout.Close(restoreFocus: false);
            OpenTaskManagerAction();
        };
        var resourceMonitorLabel = UiText.Get("ResourceMonitor");
        var resourceMonitor = new Button { Content = resourceMonitorLabel };
        resourceMonitor.SetResourceReference(StyleProperty, "WidgetFlyoutActionButton");
        AutomationProperties.SetName(resourceMonitor, resourceMonitorLabel);
        resourceMonitor.Click += (_, _) =>
        {
            _flyout.Close(restoreFocus: false);
            OpenResourceMonitorAction();
        };
        root.Children.Add(DockFlyout.FooterBar(taskManager, resourceMonitor));

        var view = new TelemetryFlyoutView
        {
            UptimeText = uptimeText,
            CpuValueText = cpuValue,
            CpuSparkline = cpuSparkline,
            RamValueText = ramValue,
            RamPairText = ramPair,
            RamSparkline = ramSparkline,
            GpuSection = gpuSection,
            GpuValueText = gpuValue,
            GpuTempText = gpuTemp,
            GpuSparkline = gpuSparkline,
            ProcessesList = processesList
        };

        RefreshTelemetryFlyout(view);

        if (isPreview)
        {
            RenderTopProcesses(processesList, _lastTopProcesses, isLoading: _lastTopProcesses.Count == 0);
            AttachPreviewOneShotProcessFetch(container, processesList);
        }
        else
        {
            _clickFlyout = view;
            RenderTopProcesses(processesList, _lastTopProcesses, isLoading: _lastTopProcesses.Count == 0);
            StartFlyoutProcessSampling();
        }

        return container;
    }

    private void AttachPreviewOneShotProcessFetch(Border container, StackPanel processesList)
    {
        CancellationTokenSource? previewCts = null;
        container.Loaded += (_, _) =>
        {
            if (_lastTopProcesses.Count > 0)
            {
                RenderTopProcesses(processesList, _lastTopProcesses, isLoading: false);
                return;
            }

            previewCts?.Cancel();
            previewCts?.Dispose();
            previewCts = new CancellationTokenSource();
            var ct = previewCts.Token;
            var reader = TopProcessesReader;
            _ = Task.Run(() =>
            {
                try { return reader(TopProcessCount, ct); }
                catch (OperationCanceledException) { return Array.Empty<ProcessMemoryEntry>(); }
            }, ct).ContinueWith(task =>
            {
                if (ct.IsCancellationRequested || !task.IsCompletedSuccessfully) return;
                _ = Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
                {
                    if (ct.IsCancellationRequested || !container.IsLoaded) return;
                    _lastTopProcesses = task.Result;
                    RenderTopProcesses(processesList, task.Result, isLoading: false);
                });
            }, TaskScheduler.Default);
        };

        container.Unloaded += (_, _) =>
        {
            if (previewCts is null) return;
            previewCts.Cancel();
            previewCts.Dispose();
            previewCts = null;
        };
    }

    private void StartFlyoutProcessSampling()
    {
        CancelProcessSampling();
        _processSampleCts = new CancellationTokenSource();
        _lastProcessSampleTickMs = Environment.TickCount64;
        QueueProcessSample(_processSampleCts.Token);
    }

    private void CancelProcessSampling()
    {
        Interlocked.Increment(ref _processGeneration);
        if (_processSampleCts is null) return;
        try { _processSampleCts.Cancel(); } catch { }
        _processSampleCts.Dispose();
        _processSampleCts = null;
    }

    private void QueueProcessSample(CancellationToken ct)
    {
        if (ct.IsCancellationRequested) return;
        if (Interlocked.CompareExchange(ref _processSampling, 1, 0) != 0) return;

        var generation = Volatile.Read(ref _processGeneration);
        var reader = TopProcessesReader;

        // Always run the delegate (do not pass ct to Task.Run) so the finally block is guaranteed to clear _processSampling.
        _ = Task.Run(() =>
        {
            IReadOnlyList<ProcessMemoryEntry> entries = [];
            try
            {
                if (ct.IsCancellationRequested || generation != Volatile.Read(ref _processGeneration))
                    return;
                entries = reader(TopProcessCount, ct);
            }
            catch (OperationCanceledException)
            {
                return;
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Top processes sampling failed: {ex.Message}");
                return;
            }
            finally
            {
                Interlocked.Exchange(ref _processSampling, 0);
            }

            if (ct.IsCancellationRequested || generation != Volatile.Read(ref _processGeneration))
                return;

            _ = Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
            {
                if (!IsActive || !_flyout.IsOpen || _clickFlyout is null
                    || generation != Volatile.Read(ref _processGeneration))
                    return;

                _lastTopProcesses = entries;
                RenderTopProcesses(_clickFlyout.ProcessesList, entries, isLoading: false);
            });
        });
    }

    private void RefreshTelemetryFlyout(TelemetryFlyoutView view)
    {
        var culture = Settings.Language.Culture;
        view.UptimeText.Text = FormatUptime(SystemStats.GetSystemUptime());

        // CPU
        var cpuRatio = _cpuState.Initialized ? _cpuState.Value : 0;
        var cpuAccent = LoadBrushKey(cpuRatio, "WidgetBlue");
        var cpuTextKey = _cpuState.Initialized ? LoadTextBrushKey(cpuRatio) : "TextPrimary";
        view.CpuValueText.Text = _cpuState.Initialized ? $"{_cpuState.Shown.ToString(CultureInfo.InvariantCulture)}%" : "—";
        view.CpuValueText.SetResourceReference(TextBlock.ForegroundProperty, cpuTextKey);
        view.CpuSparkline.SetResourceReference(TelemetrySparkline.StrokeBrushProperty, cpuAccent);
        view.CpuSparkline.UpdateFrom(_cpuHistory);

        // RAM
        var mem = _lastMemorySnapshot ?? SystemStats.SampleMemorySnapshot();
        var ramRatio = mem?.UsageRatio ?? (_memoryState.Initialized ? _memoryState.Value : 0);
        var ramShown = mem.HasValue ? (int)Math.Round(ramRatio * 100) : _memoryState.Shown;
        var ramHasValue = mem.HasValue || _memoryState.Initialized;
        var ramAccent = LoadBrushKey(ramRatio, "WidgetAccent");
        var ramTextKey = ramHasValue ? LoadTextBrushKey(ramRatio) : "TextPrimary";
        view.RamValueText.Text = ramHasValue ? $"{ramShown.ToString(CultureInfo.InvariantCulture)}%" : "—";
        view.RamValueText.SetResourceReference(TextBlock.ForegroundProperty, ramTextKey);

        view.RamPairText.Text = mem is { TotalBytes: > 0 } snap
            ? $"{FormatBytesGigabytes(snap.UsedBytes, culture)} / {FormatBytesGigabytes(snap.TotalBytes, culture)}"
            : string.Empty;

        view.RamSparkline.SetResourceReference(TelemetrySparkline.StrokeBrushProperty, ramAccent);
        view.RamSparkline.UpdateFrom(_memoryHistory);

        // GPU
        var showGpu = _hasGpu || _hasGpuTemp;
        view.GpuSection.Visibility = showGpu ? Visibility.Visible : Visibility.Collapsed;
        if (showGpu)
        {
            var gpuRatio = _gpuState.Initialized ? _gpuState.Value : 0;
            var gpuAccent = _hasGpu
                ? LoadBrushKey(gpuRatio, "AccentBrush")
                : (_hasGpuTemp && _gpuTempState.Initialized ? ThermalBrushKey(_gpuTempState.Shown) : "AccentBrush");

            view.GpuValueText.Visibility = _hasGpu ? Visibility.Visible : Visibility.Collapsed;
            if (_hasGpu)
            {
                var gpuTextKey = _gpuState.Initialized ? LoadTextBrushKey(gpuRatio) : "TextPrimary";
                view.GpuValueText.Text = _gpuState.Initialized ? $"{_gpuState.Shown.ToString(CultureInfo.InvariantCulture)}%" : "—";
                view.GpuValueText.SetResourceReference(TextBlock.ForegroundProperty, gpuTextKey);
            }

            view.GpuTempText.Visibility = _hasGpuTemp ? Visibility.Visible : Visibility.Collapsed;
            if (_hasGpuTemp)
            {
                var tempKey = _gpuTempState.Initialized ? ThermalTextBrushKey(_gpuTempState.Shown) : "TextPrimary";
                view.GpuTempText.Text = _gpuTempState.Initialized
                    ? $"{_gpuTempState.Shown.ToString(CultureInfo.InvariantCulture)}°C"
                    : "—";
                view.GpuTempText.SetResourceReference(TextBlock.ForegroundProperty, tempKey);
            }

            view.GpuSparkline.Visibility = _hasGpu ? Visibility.Visible : Visibility.Collapsed;
            if (_hasGpu)
            {
                view.GpuSparkline.SetResourceReference(TelemetrySparkline.StrokeBrushProperty, gpuAccent);
                view.GpuSparkline.UpdateFrom(_gpuHistory);
            }
        }
    }

    private void RenderTopProcesses(StackPanel container, IReadOnlyList<ProcessMemoryEntry> entries, bool isLoading)
    {
        container.Children.Clear();
        if (isLoading)
        {
            var loading = new TextBlock
            {
                Text = UiText.Get("StatsSamplingProcesses"),
                FontSize = 11,
                Margin = new Thickness(2, 4, 2, 4)
            };
            loading.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            container.Children.Add(loading);
            return;
        }

        if (entries.Count == 0)
        {
            var empty = new TextBlock
            {
                Text = UiText.Get("StatsTopProcessesEmpty"),
                FontSize = 11,
                Margin = new Thickness(2, 4, 2, 4)
            };
            empty.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            container.Children.Add(empty);
            return;
        }

        var culture = Settings.Language.Culture;
        foreach (var entry in entries)
        {
            var row = new DockPanel { Margin = new Thickness(0, 2, 0, 2), LastChildFill = true };
            var appIcon = entry.ExecutablePath is { Length: > 0 } path
                ? ApplicationIconProvider.GetForFile(path, 18)
                : null;
            if (appIcon is not null)
            {
                row.Children.Add(new Image
                {
                    Source = appIcon,
                    Width = 16,
                    Height = 16,
                    Margin = new Thickness(0, 0, 7, 0),
                    VerticalAlignment = VerticalAlignment.Center,
                    SnapsToDevicePixels = true
                });
            }
            var memory = CreateValueText();
            memory.Text = FormatProcessMemory(entry.WorkingSetBytes, culture);
            memory.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            DockPanel.SetDock(memory, Dock.Right);
            row.Children.Add(memory);
            var name = new TextBlock
            {
                Text = entry.Name,
                FontSize = 12,
                VerticalAlignment = VerticalAlignment.Center,
                TextTrimming = TextTrimming.CharacterEllipsis,
                ToolTip = entry.Name
            };
            name.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            row.Children.Add(name);
            container.Children.Add(row);
        }
    }

    private HoverFlyoutView CreateHoverFlyoutView()
    {
        var root = new StackPanel();
        var container = new Border { Width = 248, Child = root };

        var header = new DockPanel { Margin = new Thickness(0, 0, 0, 8) };
        var uptimeText = new TextBlock
        {
            FontSize = 10,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        uptimeText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        DockPanel.SetDock(uptimeText, Dock.Right);
        header.Children.Add(uptimeText);

        var title = new TextBlock
        {
            Text = UiText.Get("WidgetSystemStats"),
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center
        };
        title.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        header.Children.Add(title);
        root.Children.Add(header);

        var cpuValue = CreateValueText();
        var ramValue = CreateValueText();
        var ramDetail = CreateValueText();
        ramDetail.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        var gpuValue = CreateValueText();
        var gpuRow = CreateMetricLine(UiText.Get("Gpu"), gpuValue);

        root.Children.Add(CreateMetricLine(UiText.Get("Cpu"), cpuValue));
        root.Children.Add(CreateMetricLine(UiText.Get("Memory"), ramValue, ramDetail));
        root.Children.Add(gpuRow);

        return new HoverFlyoutView
        {
            Container = container,
            UptimeText = uptimeText,
            CpuValueText = cpuValue,
            RamValueText = ramValue,
            RamDetailText = ramDetail,
            GpuRow = gpuRow,
            GpuValueText = gpuValue
        };
    }

    private void RefreshHoverFlyout(HoverFlyoutView view)
    {
        var culture = Settings.Language.Culture;
        view.UptimeText.Text = FormatUptime(SystemStats.GetSystemUptime());

        view.CpuValueText.Text = _cpuState.Initialized ? $"{_cpuState.Shown.ToString(CultureInfo.InvariantCulture)}%" : "—";

        var mem = _lastMemorySnapshot ?? SystemStats.SampleMemorySnapshot();
        var ramRatio = mem?.UsageRatio ?? (_memoryState.Initialized ? _memoryState.Value : 0);
        var ramHas = mem.HasValue || _memoryState.Initialized;
        view.RamValueText.Text = ramHas ? $"{((int)Math.Round(ramRatio * 100)).ToString(CultureInfo.InvariantCulture)}%" : "—";
        view.RamDetailText.Text = mem is { TotalBytes: > 0 } snap
            ? $"{FormatBytesGigabytes(snap.UsedBytes, culture)} / {FormatBytesGigabytes(snap.TotalBytes, culture)}"
            : string.Empty;

        var showGpu = _hasGpu || _hasGpuTemp;
        view.GpuRow.Visibility = showGpu ? Visibility.Visible : Visibility.Collapsed;
        if (showGpu)
        {
            var parts = new List<string>(2);
            if (_hasGpu && _gpuState.Initialized) parts.Add($"{_gpuState.Shown.ToString(CultureInfo.InvariantCulture)}%");
            if (_hasGpuTemp && _gpuTempState.Initialized) parts.Add($"{_gpuTempState.Shown.ToString(CultureInfo.InvariantCulture)}°C");
            view.GpuValueText.Text = parts.Count > 0 ? string.Join(" · ", parts) : "—";
        }
    }

    private static DockPanel CreateMetricLine(string label, TextBlock value, TextBlock? detail = null)
    {
        var row = new DockPanel { Margin = new Thickness(0, 0, 0, 3) };
        DockPanel.SetDock(value, Dock.Right);
        row.Children.Add(value);
        if (detail is not null)
        {
            detail.Margin = new Thickness(8, 0, 8, 0);
            DockPanel.SetDock(detail, Dock.Right);
            row.Children.Add(detail);
        }

        var name = new TextBlock
        {
            Text = label,
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis
        };
        name.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        row.Children.Add(name);
        return row;
    }

    private static StackPanel CreateMetricSection(string label, TextBlock value, TelemetrySparkline history, TextBlock? detail = null)
    {
        var section = new StackPanel { Margin = new Thickness(0, 0, 0, 8) };
        section.Children.Add(CreateMetricLine(label, value, detail));
        section.Children.Add(history);
        return section;
    }

    private static TextBlock CreateValueText()
    {
        var text = new TextBlock
        {
            Text = "—",
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        text.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        return text;
    }

    private static TelemetrySparkline CreateSparkline(string brushKey, double height)
    {
        var sparkline = new TelemetrySparkline
        {
            Height = height
        };
        sparkline.SetResourceReference(TelemetrySparkline.StrokeBrushProperty, brushKey);
        sparkline.SetResourceReference(TelemetrySparkline.GridBrushProperty, "WidgetTrack");
        return sparkline;
    }

    internal static string LoadBrushKey(double ratio, string normalKey) => ratio switch
    {
        >= CriticalThreshold => "WidgetFocusAccent",
        >= WarningThreshold => "WidgetWarning",
        _ => normalKey
    };

    internal static string LoadTextBrushKey(double ratio) => ratio switch
    {
        >= CriticalThreshold => "WidgetDangerForeground",
        >= WarningThreshold => "WidgetWarning",
        _ => "TextPrimary"
    };

    internal static string ThermalBrushKey(double celsius) => celsius switch
    {
        >= ThermalCriticalCelsius => "WidgetFocusAccent",
        >= ThermalWarningCelsius => "WidgetWarning",
        _ => "WidgetGreen"
    };

    internal static string ThermalTextBrushKey(double celsius) => celsius switch
    {
        >= ThermalCriticalCelsius => "WidgetDangerForeground",
        >= ThermalWarningCelsius => "WidgetWarning",
        _ => "TextPrimary"
    };

    internal static string FormatBytesGigabytes(ulong bytes, CultureInfo culture)
    {
        var gb = bytes / (1024.0 * 1024.0 * 1024.0);
        var formatted = gb >= 100 ? gb.ToString("0", culture) : gb.ToString("0.0", culture);
        return $"{formatted} GB";
    }

    internal static string FormatProcessMemory(long bytes, CultureInfo culture)
    {
        if (bytes <= 0) return "0 MB";
        const double gib = 1024.0 * 1024.0 * 1024.0;
        const double mib = 1024.0 * 1024.0;
        if (bytes >= gib)
            return $"{(bytes / gib).ToString("0.0", culture)} GB";
        var mb = Math.Max(1, (long)Math.Round(bytes / mib));
        return $"{mb.ToString(CultureInfo.InvariantCulture)} MB";
    }

    internal static string FormatUptime(TimeSpan uptime)
    {
        var safe = uptime < TimeSpan.Zero ? TimeSpan.Zero : uptime;
        if (safe.Days > 0)
            return UiText.Format("StatsUptimeDaysFormat", safe.Days, safe.Hours, safe.Minutes);
        if (safe.Hours > 0)
            return UiText.Format("StatsUptimeHoursFormat", safe.Hours, safe.Minutes);
        return UiText.Format("StatsUptimeMinutesFormat", safe.Minutes);
    }
}
