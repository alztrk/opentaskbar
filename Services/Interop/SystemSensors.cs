using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.RegularExpressions;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Optional hardware readings for the system meters. Sensors are initialized on-demand only for enabled meters:
/// GPU usage uses lightweight WDDM <c>D3DKMTQueryStatistics</c> (with PDH fallback) and GPU temperature uses
/// NVIDIA NVAPI (with NVML fallback). No admin rights or kernel drivers are used.
/// </summary>
public sealed partial class SystemSensors : IDisposable
{
    private const double MaxPlausibleCelsius = 150;

    private D3dKmtGpuSampler? _d3dGpu;
    private PdhCounterArray? _pdhGpuFallback;
    private GpuThermalReader? _gpuThermal;
    private bool _gpuInitialized;
    private bool _gpuThermalInitialized;
    private bool _disposed;

    public SystemSensors() : this(SystemMeters.All)
    {
    }

    public SystemSensors(SystemMeters enabledMeters)
    {
        Configure(enabledMeters);
    }

    /// <summary>Enables only the native/COM sensors required by <paramref name="meters"/> and disposes disabled ones.</summary>
    public void Configure(SystemMeters meters)
    {
        if (_disposed) return;

        if (!meters.HasFlag(SystemMeters.Gpu))
        {
            _d3dGpu = null;
            _pdhGpuFallback?.Dispose();
            _pdhGpuFallback = null;
            _gpuInitialized = false;
        }

        if (!meters.HasFlag(SystemMeters.GpuTemperature))
        {
            _gpuThermal?.Dispose();
            _gpuThermal = null;
            _gpuThermalInitialized = false;
        }
    }

    /// <summary>Busiest engine of the busiest GPU (0–1), matching how Task Manager reports GPU usage.</summary>
    public double? SampleGpuUsage()
    {
        if (_disposed) return null;
        if (!_gpuInitialized)
        {
            _gpuInitialized = true;
            _d3dGpu = D3dKmtGpuSampler.TryCreate();
            if (_d3dGpu is null)
            {
                _pdhGpuFallback = PdhCounterArray.TryCreate(@"\GPU Engine(*)\Utilization Percentage");
            }
        }

        if (_d3dGpu is not null)
        {
            return _d3dGpu.Sample();
        }

        if (_pdhGpuFallback?.Read() is not { } samples) return null;
        var busiestEngine = samples
            .Select(sample => (Match: EngineInstance().Match(sample.Instance), sample.Value))
            .Where(item => item.Match.Success)
            .GroupBy(item => (Gpu: item.Match.Groups["luid"].Value, Engine: item.Match.Groups["engine"].Value))
            .Select(group => (group.Key.Gpu, Load: group.Sum(item => item.Value)));
        var busiestGpu = busiestEngine.GroupBy(item => item.Gpu).Select(group => group.Max(item => item.Load)).ToList();
        return busiestGpu.Count == 0 ? null : Math.Clamp(busiestGpu.Max() / 100, 0, 1);
    }

    /// <summary>Hottest NVIDIA GPU in °C; null for other vendors or when the NVIDIA driver is missing.</summary>
    public double? SampleGpuTemperature()
    {
        if (_disposed) return null;
        if (!_gpuThermalInitialized)
        {
            _gpuThermalInitialized = true;
            _gpuThermal = GpuThermalReader.TryCreate();
        }

        return _gpuThermal?.HottestGpu();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _d3dGpu = null;
        _pdhGpuFallback?.Dispose();
        _pdhGpuFallback = null;
        _gpuThermal?.Dispose();
        _gpuThermal = null;
    }

    // Instance names look like pid_1234_luid_0x00000000_0x0000F3A1_phys_0_eng_0_engtype_3D.
    [GeneratedRegex(@"_luid_(?<luid>.+?)_phys_\d+_eng_\d+_engtype_(?<engine>.+)$")]
    private static partial Regex EngineInstance();

    /// <summary>
    /// Samples per-node GPU engine running time directly via WDDM <c>D3DKMTQueryStatistics</c> in <c>gdi32.dll</c>,
    /// avoiding PDH's multi-megabyte per-process wildcard counter table.
    /// </summary>
    private sealed class D3dKmtGpuSampler
    {
        private const uint QueryAdapter = 0;
        private const uint QueryNode = 5;
        private const int QueryBufferSize = 1048;
        private const int NodeIdOffset = 800;
        private const int NodeRunningTimeOffset = 24;
        private const int AdapterNodeCountOffset = 28;

        private readonly List<(Luid Luid, uint NodeCount)> _adapters;
        private readonly long[] _previousRunningTime;
        private readonly long[] _previousNodeTimestamp;
        private readonly byte[] _buffer = new byte[QueryBufferSize];

        private D3dKmtGpuSampler(List<(Luid Luid, uint NodeCount)> adapters, int totalNodes)
        {
            _adapters = adapters;
            _previousRunningTime = new long[totalNodes];
            _previousNodeTimestamp = new long[totalNodes];
        }

        public static D3dKmtGpuSampler? TryCreate()
        {
            try
            {
                var enumArgs = new EnumAdapters2();
                if (D3DKMTEnumAdapters2(ref enumArgs) != 0 || enumArgs.NumAdapters == 0) return null;

                var infoSize = Marshal.SizeOf<AdapterInfo>();
                enumArgs.Adapters = Marshal.AllocHGlobal(infoSize * (int)enumArgs.NumAdapters);
                var adapters = new List<(Luid Luid, uint NodeCount)>();
                var totalNodes = 0;
                var buffer = new byte[QueryBufferSize];

                try
                {
                    if (D3DKMTEnumAdapters2(ref enumArgs) != 0) return null;
                    for (var i = 0; i < enumArgs.NumAdapters; i++)
                    {
                        var info = Marshal.PtrToStructure<AdapterInfo>(enumArgs.Adapters + i * infoSize);
                        try
                        {
                            Array.Clear(buffer, 0, buffer.Length);
                            WriteQueryHeader(buffer, QueryAdapter, info.AdapterLuid);
                            if (D3DKMTQueryStatistics(buffer) == 0)
                            {
                                var nodeCount = BitConverter.ToUInt32(buffer, AdapterNodeCountOffset);
                                if (nodeCount is > 0 and < 128)
                                {
                                    adapters.Add((info.AdapterLuid, nodeCount));
                                    totalNodes += (int)nodeCount;
                                }
                            }
                        }
                        finally
                        {
                            var close = new CloseAdapter { Adapter = info.Adapter };
                            D3DKMTCloseAdapter(ref close);
                        }
                    }
                }
                finally
                {
                    Marshal.FreeHGlobal(enumArgs.Adapters);
                }

                return totalNodes > 0 ? new D3dKmtGpuSampler(adapters, totalNodes) : null;
            }
            catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException)
            {
                AppLog.Info($"D3DKMT GPU counters unavailable: {ex.Message}");
                return null;
            }
        }

        public double? Sample()
        {
            var now = Stopwatch.GetTimestamp();
            var hasValidDelta = false;
            double maxUtil = 0;
            var idx = 0;
            var anySuccess = false;

            foreach (var (luid, nodeCount) in _adapters)
            {
                for (uint node = 0; node < nodeCount; node++, idx++)
                {
                    Array.Clear(_buffer, 0, _buffer.Length);
                    WriteQueryHeader(_buffer, QueryNode, luid);
                    BitConverter.TryWriteBytes(_buffer.AsSpan(NodeIdOffset, 4), node);
                    if (D3DKMTQueryStatistics(_buffer) != 0) continue;

                    anySuccess = true;
                    var runningTime = BitConverter.ToInt64(_buffer, NodeRunningTimeOffset);
                    var previousTimestamp = _previousNodeTimestamp[idx];
                    if (previousTimestamp != 0 && now > previousTimestamp)
                    {
                        var delta = runningTime - _previousRunningTime[idx];
                        var elapsed100ns = (now - previousTimestamp) * 10_000_000.0 / Stopwatch.Frequency;
                        if (delta >= 0 && elapsed100ns > 0)
                        {
                            hasValidDelta = true;
                            maxUtil = Math.Max(maxUtil, delta / elapsed100ns);
                        }
                    }

                    _previousRunningTime[idx] = runningTime;
                    _previousNodeTimestamp[idx] = now;
                }
            }

            if (!anySuccess || !hasValidDelta) return null;
            return Math.Clamp(maxUtil, 0, 1);
        }

        private static void WriteQueryHeader(byte[] buffer, uint type, Luid luid)
        {
            BitConverter.TryWriteBytes(buffer.AsSpan(0, 4), type);
            BitConverter.TryWriteBytes(buffer.AsSpan(4, 4), luid.LowPart);
            BitConverter.TryWriteBytes(buffer.AsSpan(8, 4), luid.HighPart);
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct Luid
        {
            public uint LowPart;
            public int HighPart;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct AdapterInfo
        {
            public uint Adapter;
            public Luid AdapterLuid;
            public uint NumOfSources;
            public int PresentMoveRegionsPreferred;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct EnumAdapters2
        {
            public uint NumAdapters;
            public nint Adapters;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct CloseAdapter
        {
            public uint Adapter;
        }

        [DllImport("gdi32.dll")]
        private static extern int D3DKMTEnumAdapters2(ref EnumAdapters2 arg);

        [DllImport("gdi32.dll")]
        private static extern int D3DKMTCloseAdapter(ref CloseAdapter arg);

        [DllImport("gdi32.dll")]
        private static extern int D3DKMTQueryStatistics(byte[] buffer);
    }

    private sealed class GpuThermalReader : IDisposable
    {
        private readonly bool _useD3dKmt;
        private readonly bool _nvmlReady;

        private GpuThermalReader(bool useD3dKmt, bool nvmlReady)
        {
            _useD3dKmt = useD3dKmt;
            _nvmlReady = nvmlReady;
        }

        public static GpuThermalReader? TryCreate()
        {
            // Prefer zero-overhead WDDM D3DKMTQueryAdapterInfo (resident gdi32.dll) before loading nvml.dll.
            if (SampleD3dKmtTemperature() is not null)
            {
                return new GpuThermalReader(useD3dKmt: true, nvmlReady: false);
            }

            if (Nvml.TryInitialize())
            {
                return new GpuThermalReader(useD3dKmt: false, nvmlReady: true);
            }

            return null;
        }

        public double? HottestGpu()
        {
            if (_useD3dKmt) return SampleD3dKmtTemperature();
            return _nvmlReady ? Nvml.HottestGpu() : null;
        }

        public void Dispose()
        {
            if (_nvmlReady) Nvml.Shutdown();
        }

        private static double? SampleD3dKmtTemperature()
        {
            const uint kmtAdapterPerfData = 62;
            var enumArg = new D3dkmtEnumAdapters2();
            try
            {
                if (D3DKMTEnumAdapters2(ref enumArg) != 0 || enumArg.NumAdapters == 0) return null;
            }
            catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException)
            {
                return null;
            }

            var infoSize = Marshal.SizeOf<D3dkmtAdapterInfo>();
            var buffer = Marshal.AllocHGlobal(infoSize * (int)enumArg.NumAdapters);
            var perfSize = Marshal.SizeOf<D3dkmtAdapterPerfData>();
            var perfPtr = Marshal.AllocHGlobal(perfSize);
            try
            {
                enumArg.Adapters = buffer;
                if (D3DKMTEnumAdapters2(ref enumArg) != 0) return null;

                double? hottest = null;
                for (var i = 0; i < enumArg.NumAdapters; i++)
                {
                    var info = Marshal.PtrToStructure<D3dkmtAdapterInfo>(buffer + i * infoSize);
                    try
                    {
                        Marshal.StructureToPtr(default(D3dkmtAdapterPerfData), perfPtr, fDeleteOld: false);
                        var query = new D3dkmtQueryAdapterInfo
                        {
                            Adapter = info.Adapter,
                            Type = kmtAdapterPerfData,
                            PrivateDriverData = perfPtr,
                            PrivateDriverDataSize = (uint)perfSize
                        };
                        if (D3DKMTQueryAdapterInfo(ref query) == 0)
                        {
                            var perf = Marshal.PtrToStructure<D3dkmtAdapterPerfData>(perfPtr);
                            var celsius = perf.Temperature / 10.0;
                            if (celsius is > 0 and < MaxPlausibleCelsius)
                                hottest = hottest is { } existing ? Math.Max(existing, celsius) : celsius;
                        }
                    }
                    finally
                    {
                        var close = new D3dkmtCloseAdapter { Adapter = info.Adapter };
                        D3DKMTCloseAdapter(ref close);
                    }
                }

                return hottest;
            }
            catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException)
            {
                return null;
            }
            finally
            {
                Marshal.FreeHGlobal(perfPtr);
                Marshal.FreeHGlobal(buffer);
            }
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct Luid { public uint LowPart; public int HighPart; }

        [StructLayout(LayoutKind.Sequential)]
        private struct D3dkmtAdapterInfo
        {
            public uint Adapter;
            public Luid AdapterLuid;
            public uint NumOfSources;
            [MarshalAs(UnmanagedType.Bool)] public bool PresentMoveRegionsPreferred;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct D3dkmtEnumAdapters2 { public uint NumAdapters; public nint Adapters; }

        [StructLayout(LayoutKind.Sequential)]
        private struct D3dkmtCloseAdapter { public uint Adapter; }

        [StructLayout(LayoutKind.Sequential)]
        private struct D3dkmtQueryAdapterInfo
        {
            public uint Adapter;
            public uint Type;
            public nint PrivateDriverData;
            public uint PrivateDriverDataSize;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct D3dkmtAdapterPerfData
        {
            public uint PhysicalAdapterIndex;
            public ulong MemoryFrequency;
            public ulong MaxMemoryFrequency;
            public ulong MaxMemoryFrequencyOC;
            public ulong MemoryBandwidth;
            public ulong PcieBandwidth;
            public uint FanRpm;
            public uint Power;
            public uint Temperature;
            public byte PowerStateOverride;
        }

        [DllImport("gdi32.dll")]
        private static extern int D3DKMTEnumAdapters2(ref D3dkmtEnumAdapters2 arg);

        [DllImport("gdi32.dll")]
        private static extern int D3DKMTCloseAdapter(ref D3dkmtCloseAdapter arg);

        [DllImport("gdi32.dll")]
        private static extern int D3DKMTQueryAdapterInfo(ref D3dkmtQueryAdapterInfo arg);
    }

    private static class Nvml
    {
        public static bool TryInitialize()
        {
            try
            {
                return nvmlInit_v2() == 0;
            }
            catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException)
            {
                AppLog.Info($"NVIDIA management library unavailable: {ex.Message}");
                return false;
            }
        }

        public static void Shutdown() => nvmlShutdown();

        public static double? HottestGpu()
        {
            if (nvmlDeviceGetCount_v2(out var count) != 0) return null;
            double? hottest = null;
            for (uint index = 0; index < count; index++)
            {
                if (nvmlDeviceGetHandleByIndex_v2(index, out var device) != 0
                    || nvmlDeviceGetTemperature(device, 0, out var temperature) != 0) continue;
                hottest = Math.Max(hottest ?? 0, temperature);
            }

            return hottest;
        }

        // Loaded from System32 only so a library next to the executable cannot replace the driver's.
        [DllImport("nvml.dll"), DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
        private static extern int nvmlInit_v2();

        [DllImport("nvml.dll"), DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
        private static extern int nvmlShutdown();

        [DllImport("nvml.dll"), DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
        private static extern int nvmlDeviceGetCount_v2(out uint count);

        [DllImport("nvml.dll"), DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
        private static extern int nvmlDeviceGetHandleByIndex_v2(uint index, out nint device);

        [DllImport("nvml.dll"), DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
        private static extern int nvmlDeviceGetTemperature(nint device, int sensor, out uint temperature);
    }
}
