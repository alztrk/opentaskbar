using System.IO;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using OpenTaskbar.Widgets;

namespace OpenTaskbar.Services;

/// <summary>
/// Orchestrates Bluetooth PnP and multi-protocol wireless battery providers concurrently
/// with a single-pass shared HID interface snapshot, per-provider timeouts, failure isolation,
/// wired-device filtering, deduplication, 2-sample outlier spike protection,
/// and a 30-minute smart idle/stale reading cache.
/// </summary>
internal sealed class WirelessBatteryAggregator
{
    public const ushort WraithVendorId = 0x2E3C;
    public const ushort WraithW75ProductId = 0xC368;
    public const int OutlierDropThresholdPercent = 25;
    public const int OutlierProtectedFloorPercent = 25;
    internal const int MaxCacheEntries = 64;
    internal const string CacheFileName = "wireless-battery-cache.json";
    internal const string CacheTemporaryExtension = ".json.tmp";
    private const long MaxCacheFileBytes = 65_536;

    // C# writes PascalCase + a UTF-8 BOM; Rust writes lowercase wire keys without a BOM.
    // Both shapes are accepted on read so either port can hand the cache to the other.
    private static readonly JsonSerializerOptions CacheJsonOptions = new()
    {
        PropertyNameCaseInsensitive = true,
        Converters = { new JsonStringEnumConverter() }
    };

    public static readonly TimeSpan DefaultProviderTimeout = TimeSpan.FromMilliseconds(1200);
    public static readonly TimeSpan StaleExpirationWindow = TimeSpan.FromMinutes(30);

    private static readonly WirelessBatteryAggregator Shared = new(
        bluetoothReader: BluetoothBatteryWidget.ReadLiveBluetoothReadingsAsync,
        providers:
        [
            new SteelSeriesBatteryProvider(),
            new CompxAtkVxeBatteryProvider(),
            new LogitechBatteryProvider(),
            new RazerBatteryProvider(),
            new HyperXBatteryProvider(),
            new CorsairBatteryProvider(),
            new AsusRogBatteryProvider(),
            new GamepadHidBatteryProvider(),
            new WirelessKeyboard24gProvider(),
            new StandardHidBatteryProvider(),
            new BleGattBatteryProvider(),
            new XInputBatteryProvider()
        ],
        clock: () => DateTimeOffset.UtcNow,
        providerTimeout: DefaultProviderTimeout,
        hidEnumerator: () => Win32HidTransport.EnumerateInterfaces(),
        cacheFilePath: Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "OpenTaskbar",
            CacheFileName));

    private readonly Func<CancellationToken, Task<IReadOnlyList<WirelessDeviceReading>>> _bluetoothReader;
    private readonly IReadOnlyList<IWirelessBatteryProvider> _providers;
    private readonly Func<DateTimeOffset> _clock;
    private readonly TimeSpan _providerTimeout;
    private readonly Func<IReadOnlyList<HidInterfaceDescriptor>> _hidEnumerator;
    private readonly string? _cacheFilePath;
    private readonly SemaphoreSlim _pollGate = new(1, 1);
    private readonly object _cacheLock = new();
    private readonly Dictionary<string, WirelessDeviceReading> _cache = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, int> _pendingOutliers = new(StringComparer.OrdinalIgnoreCase);
    private bool _cacheLoadedFromDisk;

    internal WirelessBatteryAggregator(
        Func<CancellationToken, Task<IReadOnlyList<WirelessDeviceReading>>> bluetoothReader,
        IReadOnlyList<IWirelessBatteryProvider> providers,
        Func<DateTimeOffset>? clock = null,
        TimeSpan? providerTimeout = null,
        Func<IReadOnlyList<HidInterfaceDescriptor>>? hidEnumerator = null,
        string? cacheFilePath = null)
    {
        _bluetoothReader = bluetoothReader;
        _providers = providers;
        _clock = clock ?? (() => DateTimeOffset.UtcNow);
        _providerTimeout = providerTimeout ?? DefaultProviderTimeout;
        _hidEnumerator = hidEnumerator ?? (() => Win32HidTransport.EnumerateInterfaces());
        _cacheFilePath = cacheFilePath;
    }

    public static async Task<IReadOnlyList<BluetoothDeviceBatteryInfo>> ReadAllAsync(bool listenOnly = false)
    {
        var readings = await Shared.PollAsync(CancellationToken.None, listenOnly).ConfigureAwait(false);
        return readings.Select(r => r.ToBatteryInfo()).ToList();
    }

    public static Task<IReadOnlyList<WirelessDeviceReading>> ReadAllReadingsAsync(
        CancellationToken cancellationToken = default,
        bool listenOnly = false) =>
        Shared.PollAsync(cancellationToken, listenOnly);

    internal static bool IsIgnoredWiredDevice(ushort vendorId, ushort productId, string? deviceName = null)
    {
        if (vendorId == WraithVendorId && (productId == WraithW75ProductId || productId == 0))
            return true;
        if (!string.IsNullOrWhiteSpace(deviceName)
            && deviceName.Contains("Wraith W75", StringComparison.OrdinalIgnoreCase))
            return true;
        return false;
    }

    internal static bool IsIgnoredWiredReading(WirelessDeviceReading reading)
    {
        if (string.IsNullOrWhiteSpace(reading.Name) && string.IsNullOrWhiteSpace(reading.Id))
            return true;
        if (IsIgnoredWiredDevice(0, 0, reading.Name))
            return true;
        if (reading.Id.Contains("2E3C", StringComparison.OrdinalIgnoreCase)
            && reading.Id.Contains("C368", StringComparison.OrdinalIgnoreCase))
            return true;
        return false;
    }

    /// <summary>
    /// Builds a strictly privacy-safe diagnostic summary containing ONLY numeric hex VID, PID,
    /// UsagePage, Usage, and report byte lengths. Never includes device paths, product names,
    /// serial numbers, MAC addresses, or user identifiers.
    /// </summary>
    internal static string BuildPrivacySafeDiagnosticSummary(IReadOnlyList<HidInterfaceDescriptor> descriptors)
    {
        if (descriptors is null || descriptors.Count == 0)
            return "HID Interfaces: 0";

        var lines = descriptors
            .Where(d => !IsIgnoredWiredDevice(d.VendorId, d.ProductId))
            .Select(d =>
                $"VID:{d.VendorId:X4} PID:{d.ProductId:X4} UsagePage:0x{d.UsagePage:X4} Usage:0x{d.Usage:X4} In:{d.InputReportLength} Out:{d.OutputReportLength} Feat:{d.FeatureReportLength}")
            .Distinct(StringComparer.Ordinal)
            .OrderBy(line => line, StringComparer.Ordinal)
            .ToList();

        if (lines.Count == 0)
            return "HID Interfaces: 0";

        var sb = new StringBuilder(lines.Count * 72);
        for (var i = 0; i < lines.Count; i++)
        {
            if (i > 0) sb.Append('\n');
            sb.Append(lines[i]);
        }
        return sb.ToString();
    }

    internal static string BuildLivePrivacySafeDiagnosticSummary()
    {
        try
        {
            var interfaces = Win32HidTransport.EnumerateInterfaces();
            return BuildPrivacySafeDiagnosticSummary(interfaces);
        }
        catch
        {
            return "HID Interfaces: 0";
        }
    }

    internal static bool IsPassiveSource(WirelessBatterySource source) => source is
        WirelessBatterySource.Bluetooth
        or WirelessBatterySource.BluetoothGatt
        or WirelessBatterySource.XInput
        or WirelessBatterySource.StandardHid
        or WirelessBatterySource.GamepadHid;

    public async Task<IReadOnlyList<WirelessDeviceReading>> PollAsync(
        CancellationToken cancellationToken = default,
        bool listenOnly = false)
    {
        await _pollGate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            IReadOnlyList<HidInterfaceDescriptor> hidSnapshot;
            try
            {
                hidSnapshot = await Task.Run(() => _hidEnumerator() ?? [], CancellationToken.None).ConfigureAwait(false);
            }
            catch
            {
                hidSnapshot = [];
            }

            var tasks = new List<Task<IReadOnlyList<WirelessDeviceReading>>>(_providers.Count + 1)
            {
                RunIsolatedAsync(_bluetoothReader, cancellationToken)
            };

            foreach (var provider in _providers)
            {
                if (listenOnly && !IsPassiveSource(provider.Source))
                    continue;
                var currentProvider = provider;
                tasks.Add(RunIsolatedAsync(token => currentProvider.ProbeAsync(hidSnapshot, token), cancellationToken));
            }

            var batches = await Task.WhenAll(tasks).ConfigureAwait(false);
            var now = _clock();

            var freshReadings = new List<WirelessDeviceReading>();
            foreach (var batch in batches)
            {
                foreach (var reading in batch)
                {
                    if (IsIgnoredWiredReading(reading)) continue;
                    freshReadings.Add(reading.SampledAt == default ? reading with { SampledAt = now } : reading);
                }
            }

            var deduplicated = DeduplicateReadings(freshReadings);
            return MergeWithCache(deduplicated, now);
        }
        finally
        {
            _pollGate.Release();
        }
    }

    private async Task<IReadOnlyList<WirelessDeviceReading>> RunIsolatedAsync(
        Func<CancellationToken, Task<IReadOnlyList<WirelessDeviceReading>>> probe,
        CancellationToken outerToken)
    {
        var softTimeoutMs = Math.Max(80.0, _providerTimeout.TotalMilliseconds * 0.8);
        var softCts = CancellationTokenSource.CreateLinkedTokenSource(outerToken);
        softCts.CancelAfter(TimeSpan.FromMilliseconds(softTimeoutMs));

        using var hardCts = CancellationTokenSource.CreateLinkedTokenSource(outerToken);
        hardCts.CancelAfter(_providerTimeout);
        Task<IReadOnlyList<WirelessDeviceReading>>? probeTask = null;
        var handedOffToContinuation = false;
        try
        {
            probeTask = Task.Run(() => probe(softCts.Token), CancellationToken.None);
            var completed = await Task.WhenAny(probeTask, Task.Delay(_providerTimeout, hardCts.Token)).ConfigureAwait(false);
            if (completed != probeTask)
            {
                // Keep softCts alive until the timed-out probe finishes so it never hits ObjectDisposedException,
                // and observe any faulted exception once it completes.
                handedOffToContinuation = true;
                _ = probeTask.ContinueWith(
                    static (t, state) =>
                    {
                        _ = t.Exception;
                        ((CancellationTokenSource)state!).Dispose();
                    },
                    softCts,
                    CancellationToken.None,
                    TaskContinuationOptions.ExecuteSynchronously,
                    TaskScheduler.Default);
                return [];
            }

            return await probeTask.ConfigureAwait(false) ?? [];
        }
        catch
        {
            return [];
        }
        finally
        {
            if (!handedOffToContinuation)
                softCts.Dispose();
        }
    }

    internal static IReadOnlyList<WirelessDeviceReading> DeduplicateReadings(IEnumerable<WirelessDeviceReading> readings)
    {
        var bySourceAndId = new List<WirelessDeviceReading>();
        var exactIndex = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);

        foreach (var candidate in readings)
        {
            var normalizedId = candidate.Id.Trim();
            if (normalizedId.Length > 0)
            {
                var exactKey = $"{candidate.Source}:{normalizedId.ToLowerInvariant()}";
                if (exactIndex.TryGetValue(exactKey, out var existingIdx))
                {
                    var existing = bySourceAndId[existingIdx];
                    bySourceAndId[existingIdx] = ShouldPreferReading(existing, candidate)
                        ? MergeMetadata(primary: candidate, secondary: existing)
                        : MergeMetadata(primary: existing, secondary: candidate);
                    continue;
                }
                exactIndex[exactKey] = bySourceAndId.Count;
            }
            bySourceAndId.Add(candidate);
        }

        var sourceNameCounts = CountBySourceAndName(bySourceAndId);
        var byCanonicalKey = new Dictionary<string, WirelessDeviceReading>(StringComparer.OrdinalIgnoreCase);

        foreach (var candidate in bySourceAndId)
        {
            var key = GetDeduplicationKey(candidate, sourceNameCounts, hasExistingSourceIdKey: false);
            if (!byCanonicalKey.TryGetValue(key, out var existing))
            {
                byCanonicalKey[key] = candidate;
                continue;
            }

            if (ShouldPreferReading(existing, candidate))
            {
                byCanonicalKey[key] = MergeMetadata(primary: candidate, secondary: existing);
            }
            else
            {
                byCanonicalKey[key] = MergeMetadata(primary: existing, secondary: candidate);
            }
        }

        return byCanonicalKey.Values.ToList();
    }

    private IReadOnlyList<WirelessDeviceReading> MergeWithCache(
        IReadOnlyList<WirelessDeviceReading> freshReadings,
        DateTimeOffset now)
    {
        Dictionary<string, WirelessDeviceReading>? snapshotToPersist = null;
        IReadOnlyList<WirelessDeviceReading> result;

        lock (_cacheLock)
        {
            EnsureCacheLoadedLocked(now);
            var cacheChanged = false;
            var sourceNameCounts = CountBySourceAndName(freshReadings);
            var activeKeys = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            var output = new List<WirelessDeviceReading>();

            foreach (var reading in freshReadings)
            {
                var idKey = $"src:{reading.Source}:id:{reading.Id.Trim().ToLowerInvariant()}";
                var key = GetDeduplicationKey(reading, sourceNameCounts, _cache.ContainsKey(idKey));
                activeKeys.Add(key);

                // -1 is the only "unknown" level; a genuine 0% non-charging device is valid data.
                var hasValidFreshBattery = reading.BatteryLevel >= 0
                                           || !string.IsNullOrWhiteSpace(reading.LevelOverrideText);

                if (reading.IsConnected && hasValidFreshBattery)
                {
                    var live = reading with
                    {
                        IsStale = false,
                        SampledAt = reading.SampledAt == default ? now : reading.SampledAt
                    };

                    // Outlier / spike guard: reject single-sample implausible drops (>= 25 percentage points
                    // in one poll from a trusted > 25% reading) unless confirmed by 2 consecutive samples.
                    if (string.IsNullOrWhiteSpace(live.LevelOverrideText)
                        && _cache.TryGetValue(key, out var prevTrusted)
                        && prevTrusted.BatteryLevel > OutlierProtectedFloorPercent
                        && prevTrusted.BatteryLevel - live.BatteryLevel >= OutlierDropThresholdPercent)
                    {
                        if (_pendingOutliers.TryGetValue(key, out var pendingLevel)
                            && prevTrusted.BatteryLevel - pendingLevel >= OutlierDropThresholdPercent)
                        {
                            // Confirmed by 2 consecutive samples: accept the new level.
                            _pendingOutliers.Remove(key);
                            cacheChanged |= UpdateCacheEntryLocked(key, live);
                            output.Add(live);
                            continue;
                        }

                        _pendingOutliers[key] = live.BatteryLevel;
                        var guarded = prevTrusted with
                        {
                            IsConnected = true,
                            IsCharging = live.IsCharging,
                            IsStale = false,
                            SampledAt = live.SampledAt,
                            SecondaryBatteryLevel = live.SecondaryBatteryLevel ?? prevTrusted.SecondaryBatteryLevel
                        };
                        cacheChanged |= UpdateCacheEntryLocked(key, guarded);
                        output.Add(guarded);
                        continue;
                    }

                    _pendingOutliers.Remove(key);
                    cacheChanged |= UpdateCacheEntryLocked(key, live);
                    output.Add(live);
                    continue;
                }

                // Never overwrite a valid cached percentage with 0% on timeout/sleep, and keep the last reading
                // marked stale while within the 30-minute expiration window.
                if (_cache.TryGetValue(key, out var cached) && now - cached.SampledAt <= StaleExpirationWindow)
                {
                    var retained = cached with
                    {
                        IsConnected = true,
                        IsCharging = reading.IsCharging,
                        IsStale = true,
                        SecondaryBatteryLevel = reading.SecondaryBatteryLevel ?? cached.SecondaryBatteryLevel
                    };
                    output.Add(retained);
                }
                else if (reading.IsConnected && reading.BatteryLevel >= 0)
                {
                    output.Add(reading);
                }
            }

            // Track active (Source, NormalizedId) pairs so promoting a device from "name:..." to "src:...:id:..."
            // never leaves its older "name:..." cache entry behind as a duplicate stale ghost.
            var activeSourceIds = new HashSet<(WirelessBatterySource Source, string NormalizedId)>();
            foreach (var reading in freshReadings)
            {
                var trimmedId = reading.Id.Trim();
                if (trimmedId.Length > 0)
                    activeSourceIds.Add((reading.Source, trimmedId.ToLowerInvariant()));
            }

            // Retain recently seen devices that went to sleep or temporarily dropped off the bus (up to 30 minutes).
            var expiredKeys = new List<string>();
            foreach (var (key, cached) in _cache)
            {
                if (now - cached.SampledAt > StaleExpirationWindow)
                {
                    expiredKeys.Add(key);
                    continue;
                }

                if (activeKeys.Contains(key)) continue;

                var cachedTrimmedId = cached.Id.Trim();
                if (cachedTrimmedId.Length > 0 && activeSourceIds.Contains((cached.Source, cachedTrimmedId.ToLowerInvariant())))
                {
                    expiredKeys.Add(key);
                    continue;
                }

                output.Add(cached with
                {
                    IsConnected = true,
                    IsCharging = false,
                    IsStale = true
                });
            }

            foreach (var key in expiredKeys)
            {
                _cache.Remove(key);
                _pendingOutliers.Remove(key);
                cacheChanged = true;
            }

            if (cacheChanged && !string.IsNullOrWhiteSpace(_cacheFilePath))
                snapshotToPersist = new Dictionary<string, WirelessDeviceReading>(_cache, StringComparer.OrdinalIgnoreCase);

            result = output
                .OrderBy(r => r.IsStale ? 1 : 0)
                .ThenBy(r => r.IsCharging ? 1 : 0)
                .ThenBy(r => r.BatteryLevel >= 0 ? r.BatteryLevel : 101)
                .ThenBy(r => GetSourcePriority(r.Source))
                .ThenBy(r => r.Name, StringComparer.OrdinalIgnoreCase)
                .ToList();
        }

        if (snapshotToPersist is not null && !string.IsNullOrWhiteSpace(_cacheFilePath))
            SaveCacheToDisk(_cacheFilePath, snapshotToPersist);

        return result;
    }

    private bool UpdateCacheEntryLocked(string key, WirelessDeviceReading live)
    {
        var changed = !_cache.TryGetValue(key, out var prev)
            || prev.BatteryLevel != live.BatteryLevel
            || prev.IsCharging != live.IsCharging
            || prev.SecondaryBatteryLevel != live.SecondaryBatteryLevel
            || !string.Equals(prev.Name, live.Name, StringComparison.Ordinal)
            || !string.Equals(prev.LevelOverrideText, live.LevelOverrideText, StringComparison.Ordinal);
        _cache[key] = live;
        return changed;
    }

    private void EnsureCacheLoadedLocked(DateTimeOffset now)
    {
        if (_cacheLoadedFromDisk) return;
        _cacheLoadedFromDisk = true;
        if (string.IsNullOrWhiteSpace(_cacheFilePath)) return;

        try
        {
            var info = new FileInfo(_cacheFilePath);
            if (!info.Exists || info.Length is <= 0 or > MaxCacheFileBytes) return;

            var json = File.ReadAllText(_cacheFilePath, Encoding.UTF8);
            using var document = JsonDocument.Parse(json);
            if (document.RootElement.ValueKind != JsonValueKind.Object) return;

            var maxFutureSkew = TimeSpan.FromMinutes(1);
            foreach (var property in document.RootElement.EnumerateObject())
            {
                if (_cache.Count >= MaxCacheEntries) break;
                var key = property.Name;
                if (string.IsNullOrWhiteSpace(key) || property.Value.ValueKind != JsonValueKind.Object) continue;
                if (!TryReadCachedReading(property.Value, now, out var entry)) continue;
                if (entry.SampledAt > now + maxFutureSkew) continue;
                if (now - entry.SampledAt > StaleExpirationWindow) continue;
                if (entry.BatteryLevel is < -1 or > 100) continue;
                if (entry.BatteryLevel < 0 && string.IsNullOrWhiteSpace(entry.LevelOverrideText)) continue;
                if (IsIgnoredWiredReading(entry)) continue;
                _cache[key] = entry;
            }
        }
        catch
        {
        }
    }

    /// <summary>
    /// Reads either on-disk shape: C#'s PascalCase snapshot or Rust's BOM-less lowercase wire form
    /// (<c>level</c>, <c>charging</c>, <c>source</c>, <c>age_secs</c>/<c>written_at</c>).
    /// </summary>
    internal static bool TryReadCachedReading(JsonElement element, DateTimeOffset now, out WirelessDeviceReading reading)
    {
        reading = default!;
        if (TryGetProperty(element, "BatteryLevel", out _))
        {
            var parsed = JsonSerializer.Deserialize<WirelessDeviceReading>(element.GetRawText(), CacheJsonOptions);
            if (parsed is null) return false;
            reading = parsed;
            return true;
        }

        if (!TryGetInt(element, "level", out var level)) return false;
        var id = TryGetString(element, "id", out var storedId) ? storedId : string.Empty;
        var name = TryGetString(element, "name", out var storedName) ? storedName : string.Empty;
        var charging = TryGetBool(element, "charging", out var isCharging) && isCharging;
        var sourceId = TryGetInt(element, "source", out var rawSource) ? rawSource : 0;
        var source = Enum.IsDefined(typeof(WirelessBatterySource), sourceId)
            ? (WirelessBatterySource)sourceId
            : WirelessBatterySource.Bluetooth;

        // Rust `written_at` is authoritative (wall clock); fall back to its relative `age_secs`.
        long ageSeconds = 0;
        if (TryGetInt64(element, "written_at", out var writtenAt))
        {
            ageSeconds = Math.Max(0, DateTimeOffset.UtcNow.ToUnixTimeSeconds() - writtenAt);
        }
        else if (TryGetInt64(element, "age_secs", out var relativeAge))
        {
            ageSeconds = Math.Max(0, relativeAge);
        }
        // Keep the full age: the caller's 30-minute stale check must reject a stale Rust
        // `written_at` even when it would have been clamped for display in the Rust reader.
        var sampledAt = now - TimeSpan.FromSeconds(ageSeconds);
        reading = new WirelessDeviceReading(
            Id: id, Name: name, Kind: BluetoothDeviceKind.Generic, BatteryLevel: level,
            IsConnected: true, IsCharging: charging, IsStale: true, Source: source,
            SampledAt: sampledAt, LevelOverrideText: level < 0 ? "-1" : null);
        return true;
    }

    private static bool TryGetProperty(JsonElement element, string name, out JsonElement value)
    {
        foreach (var property in element.EnumerateObject())
        {
            if (string.Equals(property.Name, name, StringComparison.OrdinalIgnoreCase))
            {
                value = property.Value;
                return true;
            }
        }
        value = default;
        return false;
    }

    private static bool TryGetString(JsonElement element, string name, out string value)
    {
        value = string.Empty;
        if (!TryGetProperty(element, name, out var property) || property.ValueKind != JsonValueKind.String) return false;
        value = property.GetString() ?? string.Empty;
        return true;
    }

    private static bool TryGetBool(JsonElement element, string name, out bool value)
    {
        value = false;
        if (!TryGetProperty(element, name, out var property)) return false;
        if (property.ValueKind is not (JsonValueKind.True or JsonValueKind.False)) return false;
        value = property.GetBoolean();
        return true;
    }

    private static bool TryGetInt(JsonElement element, string name, out int value)
    {
        value = 0;
        return TryGetProperty(element, name, out var property)
            && property.ValueKind == JsonValueKind.Number
            && property.TryGetInt32(out value);
    }

    private static bool TryGetInt64(JsonElement element, string name, out long value)
    {
        value = 0;
        return TryGetProperty(element, name, out var property)
            && property.ValueKind == JsonValueKind.Number
            && property.TryGetInt64(out value);
    }

    internal static string CacheTemporaryFilePath(string path) => Path.ChangeExtension(path, CacheTemporaryExtension);

    private static void SaveCacheToDisk(string path, Dictionary<string, WirelessDeviceReading> snapshot)
    {
        try
        {
            var dir = Path.GetDirectoryName(path);
            if (!string.IsNullOrWhiteSpace(dir))
                Directory.CreateDirectory(dir);

            var tmpPath = CacheTemporaryFilePath(path);
            var json = JsonSerializer.Serialize(snapshot);
            // C# keeps the UTF-8 BOM and PascalCase keys; Rust's BOM-less reader tolerates both.
            File.WriteAllText(tmpPath, json, new UTF8Encoding(encoderShouldEmitUTF8Identifier: true));
            File.Move(tmpPath, path, overwrite: true);
        }
        catch
        {
        }
    }

    private static Dictionary<(WirelessBatterySource Source, string NormalizedName), int> CountBySourceAndName(
        IEnumerable<WirelessDeviceReading> readings)
    {
        var counts = new Dictionary<(WirelessBatterySource, string), int>();
        foreach (var reading in readings)
        {
            var normalizedName = NormalizeDeviceName(reading.Name);
            if (string.IsNullOrWhiteSpace(normalizedName)) continue;
            var tuple = (reading.Source, normalizedName);
            counts[tuple] = counts.TryGetValue(tuple, out var current) ? current + 1 : 1;
        }
        return counts;
    }

    private static string GetDeduplicationKey(
        WirelessDeviceReading reading,
        IReadOnlyDictionary<(WirelessBatterySource Source, string NormalizedName), int> sourceNameCounts,
        bool hasExistingSourceIdKey)
    {
        var normalizedName = NormalizeDeviceName(reading.Name);
        if (!string.IsNullOrWhiteSpace(normalizedName))
        {
            var hasCollisionInSource = sourceNameCounts.TryGetValue((reading.Source, normalizedName), out var count) && count > 1;
            if ((hasCollisionInSource || hasExistingSourceIdKey) && !string.IsNullOrWhiteSpace(reading.Id))
                return $"src:{reading.Source}:id:{reading.Id.Trim().ToLowerInvariant()}";
            return $"name:{normalizedName}";
        }
        return $"id:{reading.Id.Trim().ToLowerInvariant()}";
    }

    internal static string GetCanonicalDeviceKey(WirelessDeviceReading reading)
    {
        // Name is the only identity shared across providers (e.g. SteelSeries HID and its GG
        // fallback report different IDs for the same physical headset), so it must be the
        // primary dedup key. Fall back to the provider ID for nameless devices.
        var normalizedName = NormalizeDeviceName(reading.Name);
        if (!string.IsNullOrWhiteSpace(normalizedName))
            return $"name:{normalizedName}";
        return $"id:{reading.Id.Trim().ToLowerInvariant()}";
    }

    private static string NormalizeDeviceName(string? name)
    {
        if (string.IsNullOrWhiteSpace(name)) return string.Empty;
        var span = name.Trim();
        return string.Join(' ', span.Split(' ', StringSplitOptions.RemoveEmptyEntries)).ToLowerInvariant();
    }

    private static bool ShouldPreferReading(WirelessDeviceReading existing, WirelessDeviceReading candidate)
    {
        if (existing.IsConnected != candidate.IsConnected)
            return candidate.IsConnected;

        var existingHasValid = existing.BatteryLevel >= 0 || !string.IsNullOrWhiteSpace(existing.LevelOverrideText);
        var candidateHasValid = candidate.BatteryLevel >= 0 || !string.IsNullOrWhiteSpace(candidate.LevelOverrideText);
        if (existingHasValid != candidateHasValid)
            return candidateHasValid;

        if (existing.IsStale != candidate.IsStale)
            return !candidate.IsStale;

        return GetSourcePriority(candidate.Source) < GetSourcePriority(existing.Source);
    }

    private static WirelessDeviceReading MergeMetadata(WirelessDeviceReading primary, WirelessDeviceReading secondary) =>
        primary with
        {
            SecondaryBatteryLevel = primary.SecondaryBatteryLevel ?? secondary.SecondaryBatteryLevel,
            VoltageMv = primary.VoltageMv ?? secondary.VoltageMv
        };

    private static int GetSourcePriority(WirelessBatterySource source) => source switch
    {
        WirelessBatterySource.SteelSeriesHid => 0,
        WirelessBatterySource.CompxAtkVxe => 1,
        WirelessBatterySource.LogitechHidPlusPlus => 2,
        WirelessBatterySource.RazerHid => 3,
        WirelessBatterySource.HyperXHid => 3,
        WirelessBatterySource.CorsairHid => 3,
        WirelessBatterySource.AsusRogHid => 3,
        WirelessBatterySource.Keyboard24gHid => 3,
        WirelessBatterySource.GamepadHid => 3,
        WirelessBatterySource.StandardHid => 4,
        WirelessBatterySource.Bluetooth => 5,
        WirelessBatterySource.BluetoothGatt => 6,
        WirelessBatterySource.XInput => 7,
        WirelessBatterySource.SteelSeriesGg => 8,
        WirelessBatterySource.LogitechGHub => 9,
        _ => 10
    };
}
