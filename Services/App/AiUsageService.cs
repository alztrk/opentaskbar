using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public interface IAiUsageProvider
{
    string Id { get; }
    string DisplayName { get; }
    Task<AiProviderReport> FetchAsync(CancellationToken cancellation, bool interactiveRefresh = false);
}

public sealed class ClaudeUsageProvider : IAiUsageProvider
{
    private static readonly Uri UsageEndpoint = new("https://api.anthropic.com/api/oauth/usage");
    private static readonly HttpClient DefaultClient = AiUsageHttp.CreateClient();

    private readonly HttpClient _client;
    private readonly string _credentialsPath;
    private readonly TimeProvider _timeProvider;

    public string Id => "claude";
    public string DisplayName => "Claude";

    public ClaudeUsageProvider()
        : this(DefaultClient, GetDefaultCredentialsPath(), TimeProvider.System)
    {
    }

    internal ClaudeUsageProvider(HttpClient client, string credentialsPath, TimeProvider timeProvider)
    {
        _client = client;
        _credentialsPath = credentialsPath;
        _timeProvider = timeProvider;
    }

    public static string GetDefaultCredentialsPath() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".claude", ".credentials.json");

    public async Task<AiProviderReport> FetchAsync(CancellationToken cancellation, bool interactiveRefresh = false)
    {
        var now = _timeProvider.GetUtcNow();
        var credentials = ReadCredentials();
        if (credentials is null)
        {
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.NotConfigured, [], now, "AiLimitsSetupHintClaude", "claude");
        }

        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, UsageEndpoint);
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", credentials.Value.AccessToken);
            request.Headers.TryAddWithoutValidation("anthropic-beta", "oauth-2025-04-20");

            using var response = await _client.SendAsync(request, cancellation);
            if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
            {
                return new AiProviderReport(Id, DisplayName, credentials.Value.PlanLabel, AiProviderStatus.Unauthorized, [], now, "AiLimitsUnauthorizedClaude", "claude");
            }

            response.EnsureSuccessStatusCode();
            var json = await response.Content.ReadAsStringAsync(cancellation);
            var windows = ParseUsageWindows(json);
            return new AiProviderReport(Id, DisplayName, credentials.Value.PlanLabel, AiProviderStatus.Ready, windows, now);
        }
        catch (Exception ex) when (AiUsageHttp.IsTransientOrParseException(ex))
        {
            AppLog.Info($"Claude usage unavailable: {ex.Message}");
            return new AiProviderReport(Id, DisplayName, credentials.Value.PlanLabel, AiProviderStatus.Error, [], now, "AiLimitsLoadError");
        }
    }

    internal static IReadOnlyList<AiUsageWindow> ParseUsageWindows(string json)
    {
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;
        var windows = new List<AiUsageWindow>(2);

        if (TryReadWindow(root, "five_hour", "AiLimitsSession", out var session))
            windows.Add(session);

        if (TryReadWindow(root, "seven_day", "AiLimitsWeekly", out var weekly))
            windows.Add(weekly);

        return windows;
    }

    private static bool TryReadWindow(JsonElement root, string propertyName, string labelKey, out AiUsageWindow window)
    {
        window = default!;
        if (!root.TryGetProperty(propertyName, out var element) || element.ValueKind != JsonValueKind.Object)
            return false;

        if (!element.TryGetProperty("utilization", out var utilProp) || utilProp.ValueKind != JsonValueKind.Number)
            return false;

        var utilization = Math.Clamp(utilProp.GetDouble(), 0, 100);
        DateTimeOffset? resetsAt = null;
        if (element.TryGetProperty("resets_at", out var resetProp)
            && resetProp.ValueKind == JsonValueKind.String
            && DateTimeOffset.TryParse(resetProp.GetString(), CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var parsedReset))
        {
            resetsAt = parsedReset;
        }

        window = new AiUsageWindow(labelKey, utilization, resetsAt);
        return true;
    }

    private (string AccessToken, string? PlanLabel)? ReadCredentials()
    {
        try
        {
            if (!File.Exists(_credentialsPath))
                return null;

            using var stream = File.OpenRead(_credentialsPath);
            using var doc = JsonDocument.Parse(stream);
            if (!doc.RootElement.TryGetProperty("claudeAiOauth", out var oauth) || oauth.ValueKind != JsonValueKind.Object)
                return null;

            if (!oauth.TryGetProperty("accessToken", out var tokenProp) || tokenProp.ValueKind != JsonValueKind.String)
                return null;

            var token = tokenProp.GetString();
            if (string.IsNullOrWhiteSpace(token))
                return null;

            string? plan = null;
            if (oauth.TryGetProperty("subscriptionType", out var subProp) && subProp.ValueKind == JsonValueKind.String)
                plan = AiUsageHttp.FormatPlanLabel(subProp.GetString());

            return (token, plan);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            AppLog.Info($"Claude credentials could not be read: {ex.Message}");
            return null;
        }
    }
}

public sealed class CodexUsageProvider : IAiUsageProvider
{
    private static readonly Uri UsageEndpoint = new("https://chatgpt.com/backend-api/wham/usage");
    private static readonly HttpClient DefaultClient = AiUsageHttp.CreateClient();

    private readonly HttpClient _client;
    private readonly string _credentialsPath;
    private readonly TimeProvider _timeProvider;

    public string Id => "codex";
    public string DisplayName => "ChatGPT";

    public CodexUsageProvider()
        : this(DefaultClient, GetDefaultCredentialsPath(), TimeProvider.System)
    {
    }

    internal CodexUsageProvider(HttpClient client, string credentialsPath, TimeProvider timeProvider)
    {
        _client = client;
        _credentialsPath = credentialsPath;
        _timeProvider = timeProvider;
    }

    public static string GetDefaultCredentialsPath() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".codex", "auth.json");

    public async Task<AiProviderReport> FetchAsync(CancellationToken cancellation, bool interactiveRefresh = false)
    {
        var now = _timeProvider.GetUtcNow();
        var credentials = ReadCredentials();
        if (credentials is null)
        {
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.NotConfigured, [], now, "AiLimitsSetupHintCodex", "codex");
        }

        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, UsageEndpoint);
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", credentials.Value.AccessToken);
            if (!string.IsNullOrWhiteSpace(credentials.Value.AccountId))
                request.Headers.TryAddWithoutValidation("ChatGPT-Account-Id", credentials.Value.AccountId);

            using var response = await _client.SendAsync(request, cancellation);
            if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
            {
                return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.Unauthorized, [], now, "AiLimitsUnauthorizedCodex", "codex");
            }

            response.EnsureSuccessStatusCode();
            var json = await response.Content.ReadAsStringAsync(cancellation);
            var (planLabel, windows) = ParseUsage(json);
            return new AiProviderReport(Id, DisplayName, planLabel, AiProviderStatus.Ready, windows, now);
        }
        catch (Exception ex) when (AiUsageHttp.IsTransientOrParseException(ex))
        {
            AppLog.Info($"ChatGPT/Codex usage unavailable: {ex.Message}");
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.Error, [], now, "AiLimitsLoadError");
        }
    }

    internal static (string? PlanLabel, IReadOnlyList<AiUsageWindow> Windows) ParseUsage(string json)
    {
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;

        string? plan = null;
        if (root.TryGetProperty("plan_type", out var planProp) && planProp.ValueKind == JsonValueKind.String)
            plan = AiUsageHttp.FormatPlanLabel(planProp.GetString());

        var windows = new List<AiUsageWindow>(2);
        if (root.TryGetProperty("rate_limit", out var rateLimit) && rateLimit.ValueKind == JsonValueKind.Object)
        {
            if (TryReadWindow(rateLimit, "primary_window", "AiLimitsSession", out var session))
                windows.Add(session);

            if (TryReadWindow(rateLimit, "secondary_window", "AiLimitsWeekly", out var weekly))
                windows.Add(weekly);
        }

        return (plan, windows);
    }

    private static bool TryReadWindow(JsonElement rateLimit, string propertyName, string labelKey, out AiUsageWindow window)
    {
        window = default!;
        if (!rateLimit.TryGetProperty(propertyName, out var element) || element.ValueKind != JsonValueKind.Object)
            return false;

        if (!element.TryGetProperty("used_percent", out var usedProp) || usedProp.ValueKind != JsonValueKind.Number)
            return false;

        var used = Math.Clamp(usedProp.GetDouble(), 0, 100);
        DateTimeOffset? resetsAt = null;
        if (element.TryGetProperty("reset_at", out var resetProp) && resetProp.ValueKind == JsonValueKind.Number)
            resetsAt = DateTimeOffset.FromUnixTimeSeconds(resetProp.GetInt64());

        window = new AiUsageWindow(labelKey, used, resetsAt);
        return true;
    }

    private (string AccessToken, string? AccountId)? ReadCredentials()
    {
        try
        {
            if (!File.Exists(_credentialsPath))
                return null;

            using var stream = File.OpenRead(_credentialsPath);
            using var doc = JsonDocument.Parse(stream);
            if (!doc.RootElement.TryGetProperty("tokens", out var tokens) || tokens.ValueKind != JsonValueKind.Object)
                return null;

            if (!tokens.TryGetProperty("access_token", out var tokenProp) || tokenProp.ValueKind != JsonValueKind.String)
                return null;

            var token = tokenProp.GetString();
            if (string.IsNullOrWhiteSpace(token))
                return null;

            string? accountId = null;
            if (tokens.TryGetProperty("account_id", out var acctProp) && acctProp.ValueKind == JsonValueKind.String)
                accountId = acctProp.GetString();

            return (token, accountId);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            AppLog.Info($"Codex credentials could not be read: {ex.Message}");
            return null;
        }
    }
}

public sealed class AntigravityUsageProvider : IAiUsageProvider
{
    private static readonly TimeSpan CliCooldown = TimeSpan.FromMinutes(15);
    private static readonly TimeSpan CliTimeout = TimeSpan.FromSeconds(15);

    private readonly Func<CancellationToken, Task<(int ExitCode, string StdOut)?>> _cliRunner;
    private readonly string _cachePath;
    private readonly TimeProvider _timeProvider;
    private DateTimeOffset _lastCliAttempt = DateTimeOffset.MinValue;
    private AiProviderReport? _cachedReport;

    public string Id => "antigravity";
    public string DisplayName => "Antigravity";

    public AntigravityUsageProvider()
        : this(RunAgyCliAsync, GetDefaultCachePath(), TimeProvider.System)
    {
    }

    internal AntigravityUsageProvider(
        Func<CancellationToken, Task<(int ExitCode, string StdOut)?>> cliRunner,
        string cachePath,
        TimeProvider timeProvider)
    {
        _cliRunner = cliRunner;
        _cachePath = cachePath;
        _timeProvider = timeProvider;
    }

    public static string GetDefaultCachePath() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenTaskbar", "antigravity-cache.json");

    public async Task<AiProviderReport> FetchAsync(CancellationToken cancellation, bool interactiveRefresh = false)
    {
        var now = _timeProvider.GetUtcNow();
        if (!interactiveRefresh && _cachedReport is not null && now - _lastCliAttempt < CliCooldown)
            return _cachedReport;

        if (!interactiveRefresh && TryLoadFreshCache(now, out var freshCache))
        {
            _lastCliAttempt = now;
            return freshCache;
        }

        if (!interactiveRefresh)
        {
            // Never spawn external CLI processes automatically on startup or background ticks;
            // only read cached data so Windows focus/overlays (Start menu, Snipping Tool) are never disrupted.
            return LoadCacheOrStatus(AiProviderStatus.NotConfigured, "AiLimitsSetupHintAntigravity", "agy", now);
        }

        _lastCliAttempt = now;
        try
        {
            var result = await Task.Run(() => _cliRunner(cancellation), cancellation);
            if (result is null)
                return LoadCacheOrStatus(AiProviderStatus.NotConfigured, "AiLimitsSetupHintAntigravity", "agy", now);

            if (result.Value.ExitCode == 0 && !string.IsNullOrWhiteSpace(result.Value.StdOut))
            {
                var (planLabel, windows) = ParseUsageJson(result.Value.StdOut, now);
                if (windows.Count > 0)
                {
                    var report = new AiProviderReport(Id, DisplayName, planLabel, AiProviderStatus.Ready, windows, now);
                    _cachedReport = report;
                    SaveCache(result.Value.StdOut);
                    return report;
                }
            }
        }
        catch (Exception ex) when (AiUsageHttp.IsTransientOrParseException(ex))
        {
            AppLog.Info($"Antigravity CLI usage unavailable: {ex.Message}");
        }

        return LoadCacheOrStatus(AiProviderStatus.Error, "AiLimitsLoadError", null, now);
    }

    private bool TryLoadFreshCache(DateTimeOffset now, out AiProviderReport report)
    {
        report = default!;
        try
        {
            if (!File.Exists(_cachePath)) return false;
            var age = now - new DateTimeOffset(File.GetLastWriteTimeUtc(_cachePath), TimeSpan.Zero);
            if (age > CliCooldown) return false;

            var json = File.ReadAllText(_cachePath);
            var (planLabel, windows) = ParseUsageJson(json, now);
            if (windows.Count == 0) return false;

            report = new AiProviderReport(Id, DisplayName, planLabel, AiProviderStatus.Ready, windows, now);
            _cachedReport = report;
            return true;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            return false;
        }
    }

    private AiProviderReport LoadCacheOrStatus(AiProviderStatus fallbackStatus, string errorKey, string? setupCommand, DateTimeOffset now)
    {
        try
        {
            if (File.Exists(_cachePath))
            {
                var json = File.ReadAllText(_cachePath);
                var (planLabel, windows) = ParseUsageJson(json, now);
                if (windows.Count > 0)
                {
                    var cached = new AiProviderReport(Id, DisplayName, planLabel, AiProviderStatus.Ready, windows, now);
                    _cachedReport = cached;
                    return cached;
                }
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            AppLog.Info($"Antigravity cache could not be read: {ex.Message}");
        }

        return new AiProviderReport(Id, DisplayName, null, fallbackStatus, [], now, errorKey, setupCommand);
    }

    private void SaveCache(string json)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(_cachePath)!);
            File.WriteAllText(_cachePath, json);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Ignore non-critical cache write errors.
        }
    }

    internal static (string? PlanLabel, IReadOnlyList<AiUsageWindow> Windows) ParseUsageJson(string json, DateTimeOffset now)
    {
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;

        if (root.TryGetProperty("command", out var command)
            && command.ValueKind == JsonValueKind.Object
            && command.TryGetProperty("data", out var data)
            && data.ValueKind == JsonValueKind.Object
            && data.TryGetProperty("groups", out var groups)
            && groups.ValueKind == JsonValueKind.Array)
        {
            return (null, ParseCommandGroups(groups, now));
        }

        string? plan = null;
        if (root.TryGetProperty("userTier", out var tier) && tier.ValueKind == JsonValueKind.Object
            && tier.TryGetProperty("description", out var desc) && desc.ValueKind == JsonValueKind.String)
        {
            plan = desc.GetString();
        }
        else if (root.TryGetProperty("plan", out var planProp) && planProp.ValueKind == JsonValueKind.String)
        {
            plan = AiUsageHttp.FormatPlanLabel(planProp.GetString());
        }

        if (root.TryGetProperty("buckets", out var buckets) && buckets.ValueKind == JsonValueKind.Array)
            return (plan, ParseBuckets(buckets, now));

        var windows = new List<AiUsageWindow>(2);
        if (TryReadPercentWindow(root, "five_hour", "AiLimitsSession", windows)
            | TryReadPercentWindow(root, "seven_day", "AiLimitsWeekly", windows))
        {
            return (plan, windows);
        }

        return (plan, []);
    }

    private static IReadOnlyList<AiUsageWindow> ParseCommandGroups(JsonElement groups, DateTimeOffset now)
    {
        JsonElement? targetGroup = null;
        foreach (var group in groups.EnumerateArray())
        {
            if (group.ValueKind != JsonValueKind.Object) continue;
            var name = group.TryGetProperty("name", out var nameProp) && nameProp.ValueKind == JsonValueKind.String
                ? nameProp.GetString()
                : null;

            if (targetGroup is null)
                targetGroup = group;

            if (name is not null && name.Contains("Gemini Models", StringComparison.OrdinalIgnoreCase))
            {
                targetGroup = group;
                break;
            }
        }

        if (targetGroup is not { } selected
            || !selected.TryGetProperty("buckets", out var buckets)
            || buckets.ValueKind != JsonValueKind.Array)
        {
            return [];
        }

        AiUsageWindow? session = null;
        AiUsageWindow? weekly = null;

        foreach (var bucket in buckets.EnumerateArray())
        {
            if (!bucket.TryGetProperty("remaining_fraction", out var remProp) || remProp.ValueKind != JsonValueKind.Number)
                continue;

            var used = Math.Clamp((1.0 - remProp.GetDouble()) * 100.0, 0, 100);
            DateTimeOffset? resetsAt = null;
            if (bucket.TryGetProperty("reset_time", out var resetProp)
                && resetProp.ValueKind == JsonValueKind.String
                && DateTimeOffset.TryParse(resetProp.GetString(), CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var parsed))
            {
                if (parsed <= now)
                    used = 0;
                else if (used >= 0.5)
                    resetsAt = parsed;
            }

            var windowType = bucket.TryGetProperty("window", out var winProp) && winProp.ValueKind == JsonValueKind.String
                ? winProp.GetString()
                : null;

            if (string.Equals(windowType, "5h", StringComparison.OrdinalIgnoreCase))
                session = new AiUsageWindow("AiLimitsSession", used, resetsAt);
            else if (string.Equals(windowType, "weekly", StringComparison.OrdinalIgnoreCase))
                weekly = new AiUsageWindow("AiLimitsWeekly", used, resetsAt);
        }

        var windows = new List<AiUsageWindow>(2);
        if (session is not null) windows.Add(session);
        if (weekly is not null) windows.Add(weekly);
        return windows;
    }

    private static IReadOnlyList<AiUsageWindow> ParseBuckets(JsonElement buckets, DateTimeOffset now)
    {
        AiUsageWindow? proPeak = null;
        AiUsageWindow? flashPeak = null;

        foreach (var bucket in buckets.EnumerateArray())
        {
            if (!bucket.TryGetProperty("remainingFraction", out var remProp) || remProp.ValueKind != JsonValueKind.Number)
                continue;

            var used = Math.Clamp((1.0 - remProp.GetDouble()) * 100.0, 0, 100);
            DateTimeOffset? resetsAt = null;
            if (bucket.TryGetProperty("resetTime", out var resetProp)
                && resetProp.ValueKind == JsonValueKind.String
                && DateTimeOffset.TryParse(resetProp.GetString(), CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var parsed))
            {
                if (parsed <= now)
                    used = 0;
                else if (used >= 0.5)
                    resetsAt = parsed;
            }

            var modelId = bucket.TryGetProperty("modelId", out var modelProp) && modelProp.ValueKind == JsonValueKind.String
                ? modelProp.GetString() ?? string.Empty
                : string.Empty;

            if (modelId.Contains("flash", StringComparison.OrdinalIgnoreCase))
            {
                if (flashPeak is null || used > flashPeak.UtilizationPercent)
                    flashPeak = new AiUsageWindow("AiLimitsFlash", used, resetsAt);
            }
            else
            {
                if (proPeak is null || used > proPeak.UtilizationPercent)
                    proPeak = new AiUsageWindow("AiLimitsPro", used, resetsAt);
            }
        }

        var windows = new List<AiUsageWindow>(2);
        if (proPeak is not null) windows.Add(proPeak);
        if (flashPeak is not null) windows.Add(flashPeak);
        return windows;
    }

    private static bool TryReadPercentWindow(JsonElement root, string propertyName, string labelKey, List<AiUsageWindow> target)
    {
        if (!root.TryGetProperty(propertyName, out var element) || element.ValueKind != JsonValueKind.Object)
            return false;
        if (!element.TryGetProperty("utilization", out var utilProp) || utilProp.ValueKind != JsonValueKind.Number)
            return false;

        var used = Math.Clamp(utilProp.GetDouble(), 0, 100);
        DateTimeOffset? resetsAt = null;
        if (element.TryGetProperty("resets_at", out var resetProp)
            && resetProp.ValueKind == JsonValueKind.String
            && DateTimeOffset.TryParse(resetProp.GetString(), CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var parsed))
        {
            resetsAt = parsed;
        }

        target.Add(new AiUsageWindow(labelKey, used, resetsAt));
        return true;
    }

    private static async Task<(int ExitCode, string StdOut)?> RunAgyCliAsync(CancellationToken cancellation)
    {
        var executable = ResolveAgyExecutable();
        if (executable is null) return null;

        using var process = new Process
        {
            StartInfo = new ProcessStartInfo
            {
                FileName = executable,
                Arguments = "-p /usage --output-format json",
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
                WindowStyle = ProcessWindowStyle.Hidden,
                ErrorDialog = false
            }
        };

        if (!process.Start()) return null;

        using var timeoutCts = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        timeoutCts.CancelAfter(CliTimeout);

        try
        {
            var output = await process.StandardOutput.ReadToEndAsync(timeoutCts.Token);
            await process.WaitForExitAsync(timeoutCts.Token);
            return (process.ExitCode, output);
        }
        catch (OperationCanceledException) when (!cancellation.IsCancellationRequested)
        {
            try { process.Kill(entireProcessTree: true); } catch { /* ignore */ }
            return (-1, string.Empty);
        }
    }

    private static string? ResolveAgyExecutable()
    {
        var bundled = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".gemini", "bin", "agy.exe");
        if (File.Exists(bundled)) return bundled;

        var pathEnv = Environment.GetEnvironmentVariable("PATH");
        if (string.IsNullOrWhiteSpace(pathEnv)) return null;

        foreach (var dir in pathEnv.Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries))
        {
            var candidate = Path.Combine(dir.Trim(), "agy.exe");
            if (File.Exists(candidate)) return candidate;
            var cmdCandidate = Path.Combine(dir.Trim(), "agy.cmd");
            if (File.Exists(cmdCandidate)) return cmdCandidate;
        }

        return null;
    }
}

public sealed class CopilotUsageProvider : IAiUsageProvider
{
    private static readonly Uri UsageEndpoint = new("https://api.github.com/copilot_internal/user");
    private static readonly HttpClient DefaultClient = AiUsageHttp.CreateClient();

    private readonly HttpClient _client;
    private readonly IReadOnlyList<string> _candidatePaths;
    private readonly TimeProvider _timeProvider;

    public string Id => "copilot";
    public string DisplayName => "GitHub Copilot";

    public CopilotUsageProvider()
        : this(DefaultClient, GetDefaultCandidatePaths(), TimeProvider.System)
    {
    }

    internal CopilotUsageProvider(HttpClient client, IReadOnlyList<string> candidatePaths, TimeProvider timeProvider)
    {
        _client = client;
        _candidatePaths = candidatePaths;
        _timeProvider = timeProvider;
    }

    public static IReadOnlyList<string> GetDefaultCandidatePaths() =>
    [
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "github-copilot", "apps.json"),
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".config", "github-copilot", "apps.json")
    ];

    public async Task<AiProviderReport> FetchAsync(CancellationToken cancellation, bool interactiveRefresh = false)
    {
        var now = _timeProvider.GetUtcNow();
        var token = ReadOAuthToken();
        if (token is null)
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.NotConfigured, [], now, "AiLimitsSetupHintCopilot");

        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, UsageEndpoint);
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            request.Headers.TryAddWithoutValidation("Accept", "application/vnd.github+json");

            using var response = await _client.SendAsync(request, cancellation);
            if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
                return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.Unauthorized, [], now, "AiLimitsUnauthorizedCopilot");

            response.EnsureSuccessStatusCode();
            var json = await response.Content.ReadAsStringAsync(cancellation);
            var (planLabel, windows) = ParseUsage(json);
            return new AiProviderReport(Id, DisplayName, planLabel, AiProviderStatus.Ready, windows, now);
        }
        catch (Exception ex) when (AiUsageHttp.IsTransientOrParseException(ex))
        {
            AppLog.Info($"Copilot usage unavailable: {ex.Message}");
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.Error, [], now, "AiLimitsLoadError");
        }
    }

    internal static (string? PlanLabel, IReadOnlyList<AiUsageWindow> Windows) ParseUsage(string json)
    {
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;

        string? plan = null;
        if (root.TryGetProperty("copilot_plan", out var planProp) && planProp.ValueKind == JsonValueKind.String)
            plan = AiUsageHttp.FormatPlanLabel(planProp.GetString());

        DateTimeOffset? resetDate = null;
        if (root.TryGetProperty("quota_reset_date_utc", out var resetProp)
            && resetProp.ValueKind == JsonValueKind.String
            && DateTimeOffset.TryParse(resetProp.GetString(), CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var parsedReset))
        {
            resetDate = parsedReset;
        }

        var windows = new List<AiUsageWindow>(1);
        if (root.TryGetProperty("quota_snapshots", out var snapshots)
            && snapshots.ValueKind == JsonValueKind.Object
            && snapshots.TryGetProperty("premium_interactions", out var premium)
            && premium.ValueKind == JsonValueKind.Object
            && premium.TryGetProperty("percent_remaining", out var remProp)
            && remProp.ValueKind == JsonValueKind.Number)
        {
            var used = Math.Clamp(100.0 - remProp.GetDouble(), 0, 100);
            windows.Add(new AiUsageWindow("AiLimitsMonthly", used, resetDate));
        }

        return (plan, windows);
    }

    private string? ReadOAuthToken()
    {
        foreach (var path in _candidatePaths)
        {
            try
            {
                if (!File.Exists(path)) continue;
                using var stream = File.OpenRead(path);
                using var doc = JsonDocument.Parse(stream);
                if (doc.RootElement.ValueKind != JsonValueKind.Object) continue;

                foreach (var entry in doc.RootElement.EnumerateObject())
                {
                    if (entry.Value.ValueKind == JsonValueKind.Object
                        && entry.Value.TryGetProperty("oauth_token", out var tokenProp)
                        && tokenProp.ValueKind == JsonValueKind.String)
                    {
                        var token = tokenProp.GetString();
                        if (!string.IsNullOrWhiteSpace(token))
                            return token;
                    }
                }
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
            {
                AppLog.Info($"Copilot credentials could not be read: {ex.Message}");
            }
        }

        return null;
    }
}

public sealed class DeepSeekUsageProvider : IAiUsageProvider
{
    private static readonly Uri BalanceEndpoint = new("https://api.deepseek.com/user/balance");
    private static readonly HttpClient DefaultClient = AiUsageHttp.CreateClient();

    private readonly HttpClient _client;
    private readonly Func<string?> _apiKeyReader;
    private readonly TimeProvider _timeProvider;

    public string Id => "deepseek";
    public string DisplayName => "DeepSeek";

    public DeepSeekUsageProvider()
        : this(DefaultClient, () => AiUsageHttp.ReadEnvironmentVariable("DEEPSEEK_API_KEY"), TimeProvider.System)
    {
    }

    internal DeepSeekUsageProvider(HttpClient client, Func<string?> apiKeyReader, TimeProvider timeProvider)
    {
        _client = client;
        _apiKeyReader = apiKeyReader;
        _timeProvider = timeProvider;
    }

    public async Task<AiProviderReport> FetchAsync(CancellationToken cancellation, bool interactiveRefresh = false)
    {
        var now = _timeProvider.GetUtcNow();
        var apiKey = _apiKeyReader();
        if (string.IsNullOrWhiteSpace(apiKey))
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.NotConfigured, [], now, "AiLimitsSetupHintDeepSeek");

        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, BalanceEndpoint);
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", apiKey.Trim());

            using var response = await _client.SendAsync(request, cancellation);
            if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
                return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.Unauthorized, [], now, "AiLimitsUnauthorizedApiKey");

            response.EnsureSuccessStatusCode();
            var json = await response.Content.ReadAsStringAsync(cancellation);
            var (planLabel, windows) = ParseBalance(json);
            return new AiProviderReport(Id, DisplayName, planLabel, AiProviderStatus.Ready, windows, now);
        }
        catch (Exception ex) when (AiUsageHttp.IsTransientOrParseException(ex))
        {
            AppLog.Info($"DeepSeek balance unavailable: {ex.Message}");
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.Error, [], now, "AiLimitsLoadError");
        }
    }

    internal static (string? PlanLabel, IReadOnlyList<AiUsageWindow> Windows) ParseBalance(string json)
    {
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;

        if (!root.TryGetProperty("balance_infos", out var infos) || infos.ValueKind != JsonValueKind.Array)
            return (null, []);

        foreach (var info in infos.EnumerateArray())
        {
            if (info.ValueKind != JsonValueKind.Object) continue;
            var currency = info.TryGetProperty("currency", out var curProp) && curProp.ValueKind == JsonValueKind.String
                ? curProp.GetString() ?? "USD"
                : "USD";

            var totalStr = info.TryGetProperty("total_balance", out var totProp) && totProp.ValueKind == JsonValueKind.String
                ? totProp.GetString()
                : null;
            if (!double.TryParse(totalStr, NumberStyles.Float, CultureInfo.InvariantCulture, out var totalBalance))
                continue;

            var balanceText = $"{totalBalance.ToString("0.##", CultureInfo.InvariantCulture)} {currency}";
            var usedPercent = totalBalance <= 0 ? 100.0 : 0.0;
            return (null, [new AiUsageWindow("AiLimitsRemainingBalance", usedPercent, null, balanceText)]);
        }

        return (null, []);
    }
}

public sealed class OpenRouterUsageProvider : IAiUsageProvider
{
    private static readonly Uri CreditsEndpoint = new("https://openrouter.ai/api/v1/credits");
    private static readonly HttpClient DefaultClient = AiUsageHttp.CreateClient();

    private readonly HttpClient _client;
    private readonly Func<string?> _apiKeyReader;
    private readonly TimeProvider _timeProvider;

    public string Id => "openrouter";
    public string DisplayName => "OpenRouter";

    public OpenRouterUsageProvider()
        : this(DefaultClient, () => AiUsageHttp.ReadEnvironmentVariable("OPENROUTER_API_KEY"), TimeProvider.System)
    {
    }

    internal OpenRouterUsageProvider(HttpClient client, Func<string?> apiKeyReader, TimeProvider timeProvider)
    {
        _client = client;
        _apiKeyReader = apiKeyReader;
        _timeProvider = timeProvider;
    }

    public async Task<AiProviderReport> FetchAsync(CancellationToken cancellation, bool interactiveRefresh = false)
    {
        var now = _timeProvider.GetUtcNow();
        var apiKey = _apiKeyReader();
        if (string.IsNullOrWhiteSpace(apiKey))
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.NotConfigured, [], now, "AiLimitsSetupHintOpenRouter");

        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, CreditsEndpoint);
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", apiKey.Trim());

            using var response = await _client.SendAsync(request, cancellation);
            if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
                return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.Unauthorized, [], now, "AiLimitsUnauthorizedApiKey");

            response.EnsureSuccessStatusCode();
            var json = await response.Content.ReadAsStringAsync(cancellation);
            var (planLabel, windows) = ParseCredits(json);
            return new AiProviderReport(Id, DisplayName, planLabel, AiProviderStatus.Ready, windows, now);
        }
        catch (Exception ex) when (AiUsageHttp.IsTransientOrParseException(ex))
        {
            AppLog.Info($"OpenRouter credits unavailable: {ex.Message}");
            return new AiProviderReport(Id, DisplayName, null, AiProviderStatus.Error, [], now, "AiLimitsLoadError");
        }
    }

    internal static (string? PlanLabel, IReadOnlyList<AiUsageWindow> Windows) ParseCredits(string json)
    {
        using var doc = JsonDocument.Parse(json);
        if (!doc.RootElement.TryGetProperty("data", out var data) || data.ValueKind != JsonValueKind.Object)
            return (null, []);

        if (!data.TryGetProperty("total_credits", out var totalProp) || totalProp.ValueKind != JsonValueKind.Number
            || !data.TryGetProperty("total_usage", out var usedProp) || usedProp.ValueKind != JsonValueKind.Number)
        {
            return (null, []);
        }

        var totalCredits = totalProp.GetDouble();
        var totalUsage = usedProp.GetDouble();
        var remaining = Math.Max(0, totalCredits - totalUsage);
        var planLabel = $"${remaining.ToString("0.##", CultureInfo.InvariantCulture)}";
        var usedPercent = totalCredits > 0 ? Math.Clamp(totalUsage / totalCredits * 100.0, 0, 100) : 0;
        return (planLabel, [new AiUsageWindow("AiLimitsCredits", usedPercent, null)]);
    }
}

internal static class AiUsageHttp
{
    public static HttpClient CreateClient()
    {
        var client = new HttpClient { Timeout = TimeSpan.FromSeconds(10) };
        client.DefaultRequestHeaders.UserAgent.ParseAdd("OpenTaskbar");
        return client;
    }

    public static bool IsTransientOrParseException(Exception ex) =>
        ex is HttpRequestException or TaskCanceledException or OperationCanceledException
            or TimeoutException or JsonException or InvalidOperationException or FormatException;

    public static string? FormatPlanLabel(string? raw)
    {
        if (string.IsNullOrWhiteSpace(raw)) return null;
        var trimmed = raw.Trim();
        return char.ToUpperInvariant(trimmed[0]) + trimmed[1..];
    }

    public static string? ReadEnvironmentVariable(string name)
    {
        var value = Environment.GetEnvironmentVariable(name);
        if (!string.IsNullOrWhiteSpace(value)) return value;
        value = Environment.GetEnvironmentVariable(name, EnvironmentVariableTarget.User);
        if (!string.IsNullOrWhiteSpace(value)) return value;
        value = Environment.GetEnvironmentVariable(name, EnvironmentVariableTarget.Machine);
        return string.IsNullOrWhiteSpace(value) ? null : value;
    }
}

public static class AiUsageService
{
    private static readonly IAiUsageProvider[] DefaultProviders =
    [
        new ClaudeUsageProvider(),
        new CodexUsageProvider(),
        new AntigravityUsageProvider(),
        new CopilotUsageProvider(),
        new DeepSeekUsageProvider(),
        new OpenRouterUsageProvider()
    ];

    public static async Task<IReadOnlyList<AiProviderReport>> LoadAllAsync(CancellationToken cancellation, bool interactiveRefresh = false)
    {
        var tasks = DefaultProviders.Select(provider => provider.FetchAsync(cancellation, interactiveRefresh));
        return await Task.WhenAll(tasks);
    }
}
