using System.Collections.Concurrent;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Net.NetworkInformation;
using System.Runtime.InteropServices;
using System.Security;
using System.Text;
using System.Text.Json;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>One quota window; Severity is the provider's own level ("normal", "warning", ...) when it sends one.</summary>
public sealed record AiUsageWindow(string LabelKey, double Used, DateTimeOffset? ResetsAt, long? WindowSeconds, string? Severity = null, string? Group = null, string? GroupModels = null);
public sealed record AiUsageShare(string Name, double Used);
public sealed record AiUsageSpend(decimal Used, decimal? Limit, string Currency, double? Used01);

public sealed record AiUsageReport(AiProviders Provider, string? Plan, IReadOnlyList<AiUsageWindow> Windows, DateTimeOffset FetchedAt)
{
    public bool LimitReached { get; init; }
    /// <summary>Claude weekly usage split by surface, as a share of the weekly limit; only non-zero rows.</summary>
    public IReadOnlyList<AiUsageShare> Breakdown { get; init; } = [];
    public AiUsageSpend? Spend { get; init; }
    /// <summary>Codex credit balance; null when the account has no credits.</summary>
    public double? Credits { get; init; }
    public bool UnlimitedCredits { get; init; }
    public int ResetCredits { get; init; }
}

/// <summary>Borrowed CLI sign-in, read on each poll. Never refreshes, writes or caches credentials.</summary>
public static class AiUsageService
{
    private static readonly HttpClient Client = CreateClient();
    private const double MaxResetSeconds = 4e9;
    private const string ClaudeOAuthClientId = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    internal sealed record LoadResult(AiUsageReport? Report, string? Error, TimeSpan? RetryAfter = null);
    private sealed record Credential(string Token, string? Account = null, string? Plan = null, string? RefreshToken = null, string? FilePath = null);

    public static event Action<AiProviders>? CredentialsChanged;

    public static void NotifyCredentialsChanged(AiProviders provider) => CredentialsChanged?.Invoke(provider);

    public static bool IsApiKeyProvider(AiProviders provider) =>
        provider is AiProviders.DeepSeek or AiProviders.OpenRouter or AiProviders.OpenCode;

    public static bool IsCliSignInProvider(AiProviders provider) =>
        provider is AiProviders.Claude or AiProviders.Codex or AiProviders.Copilot or AiProviders.OpenCode;

    internal static string SecretKey(AiProviders provider) => provider switch
    {
        AiProviders.DeepSeek => "api-key:deepseek",
        AiProviders.OpenRouter => "api-key:openrouter",
        AiProviders.OpenCode => "api-key:opencode",
        _ => $"api-key:{provider.ToString().ToLowerInvariant()}"
    };

    public static bool HasCredential(AiProviders provider) =>
        ReadCredential(provider, allowCliProbe: false) is not null
        || (provider == AiProviders.OpenCode && HasOpenCodeDatabase());

    private static bool HasOpenCodeDatabase()
    {
        try
        {
            var dbPath = Path.Combine(Path.GetDirectoryName(CredentialPath(AiProviders.OpenCode)) ?? string.Empty, "opencode.db");
            return File.Exists(dbPath);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException
            or NotSupportedException or SecurityException)
        {
            return false;
        }
    }

    public static bool HasSavedSecret(AiProviders provider) =>
        IsApiKeyProvider(provider) && AiSecretStore.HasSecret(SecretKey(provider));

    public static void SaveApiKey(AiProviders provider, string? key)
    {
        if (!IsApiKeyProvider(provider)) return;
        AiSecretStore.Set(SecretKey(provider), key);
        NotifyCredentialsChanged(provider);
    }

    public static string? CliSignInCommand(AiProviders provider) => provider switch
    {
        AiProviders.Claude => "claude auth login",
        AiProviders.Codex => "codex login",
        AiProviders.Copilot => "gh auth login",
        AiProviders.OpenCode => "opencode auth login",
        _ => null
    };

    private static string? CliExecutableName(AiProviders provider) => provider switch
    {
        AiProviders.Claude => "claude",
        AiProviders.Codex => "codex",
        AiProviders.Copilot => "gh",
        AiProviders.OpenCode => "opencode",
        _ => null
    };

    private static string[]? CliSignInArguments(AiProviders provider) => provider switch
    {
        AiProviders.Claude => ["claude", "auth", "login"],
        AiProviders.Codex => ["codex", "login"],
        AiProviders.Copilot => ["gh", "auth", "login"],
        AiProviders.OpenCode => ["opencode", "auth", "login"],
        _ => null
    };

    private static readonly ConcurrentDictionary<AiProviders, CancellationTokenSource> SignInWatchers = new();

    internal static ProcessStartInfo? BuildSignInStartInfo(AiProviders provider) =>
        BuildSignInStartInfo(provider, FindExecutableOnPath("wt"));

    internal static ProcessStartInfo? BuildSignInStartInfo(AiProviders provider, string? windowsTerminalPath)
    {
        if (CliSignInArguments(provider) is not { } args || CliSignInCommand(provider) is not { } command)
            return null;

        if (!string.IsNullOrWhiteSpace(windowsTerminalPath))
        {
            var wtInfo = new ProcessStartInfo(windowsTerminalPath)
            {
                UseShellExecute = false,
                CreateNoWindow = false,
                WindowStyle = ProcessWindowStyle.Normal
            };
            wtInfo.ArgumentList.Add("-w");
            wtInfo.ArgumentList.Add("new");
            wtInfo.ArgumentList.Add("--");
            wtInfo.ArgumentList.Add("cmd.exe");
            wtInfo.ArgumentList.Add("/c");
            foreach (var arg in args) wtInfo.ArgumentList.Add(arg);
            return wtInfo;
        }

        return new ProcessStartInfo("cmd.exe")
        {
            UseShellExecute = true,
            WindowStyle = ProcessWindowStyle.Normal,
            Arguments = $"/c {command}"
        };
    }

    internal static Func<ProcessStartInfo, bool>? ProcessLauncherOverride { get; set; }

    internal static string? FindExecutableOnPath(string command)
    {
        if (string.IsNullOrWhiteSpace(command)) return null;
        var pathEnv = Environment.GetEnvironmentVariable("PATH");
        if (string.IsNullOrWhiteSpace(pathEnv)) return null;
        string[] extensions = [".exe", ".cmd", ".bat"];
        foreach (var dir in pathEnv.Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            try
            {
                foreach (var ext in extensions)
                {
                    var candidate = Path.Combine(dir, command + ext);
                    if (File.Exists(candidate)) return candidate;
                }
            }
            catch (Exception ex) when (ex is ArgumentException or IOException or UnauthorizedAccessException or NotSupportedException) { }
        }
        return null;
    }

    public static string? TryLaunchSignIn(AiProviders provider)
    {
        if (BuildSignInStartInfo(provider) is not { } startInfo || CliExecutableName(provider) is not { } exeName)
            return "ErrorAiUsageSignIn";

        if (ProcessLauncherOverride is { } launcher)
        {
            try
            {
                return launcher(startInfo) ? null : "ErrorAiUsageSignIn";
            }
            catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or IOException or InvalidOperationException)
            {
                return "ErrorAiUsageSignIn";
            }
        }

        if (FindExecutableOnPath(exeName) is null)
            return "ErrorAiUsageSignIn";

        var initialStamp = CredentialStamp(provider);
        var initiallyConnected = HasCredential(provider);

        try
        {
            var process = Process.Start(startInfo);
            if (process is null) return "ErrorAiUsageSignIn";
            _ = process.WaitForExitAsync().ContinueWith(task =>
            {
                _ = task.Exception;
                process.Dispose();
                NotifyCredentialsChanged(provider);
            }, TaskScheduler.Default);
            StartSignInWatcher(provider, initialStamp, initiallyConnected);
            return null;
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or IOException or InvalidOperationException)
        {
            return "ErrorAiUsageSignIn";
        }
    }

    private static void StartSignInWatcher(AiProviders provider, DateTime initialStamp, bool initiallyConnected)
    {
        var next = new CancellationTokenSource();
        next.CancelAfter(TimeSpan.FromMinutes(3));
        SignInWatchers.AddOrUpdate(provider, next, (_, existing) =>
        {
            try { existing.Cancel(); } catch (ObjectDisposedException) { }
            return next;
        });

        _ = Task.Run(async () =>
        {
            try
            {
                using var timer = new PeriodicTimer(TimeSpan.FromSeconds(2));
                while (await timer.WaitForNextTickAsync(next.Token).ConfigureAwait(false))
                {
                    if (CredentialStamp(provider) != initialStamp || (!initiallyConnected && HasCredential(provider)))
                    {
                        NotifyCredentialsChanged(provider);
                        break;
                    }
                }
            }
            catch (OperationCanceledException) when (next.IsCancellationRequested) { }
            finally
            {
                SignInWatchers.TryRemove(new KeyValuePair<AiProviders, CancellationTokenSource>(provider, next));
                next.Dispose();
            }
        });
    }

    // Keep background quota queries non-interactive and out of the foreground.
    // CreateNoWindow applies to a console executable; Hidden also requests that
    // Windows not show its initial window if the target has a window subsystem.
    internal static ProcessStartInfo HiddenCliStartInfo(string executable, params string[] arguments)
    {
        var resolved = Path.IsPathRooted(executable) ? executable : (FindExecutableOnPath(executable) ?? executable);
        var startInfo = new ProcessStartInfo(resolved)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            WindowStyle = ProcessWindowStyle.Hidden,
            WorkingDirectory = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = Encoding.UTF8
        };
        foreach (var argument in arguments) startInfo.ArgumentList.Add(argument);
        return startInfo;
    }


    public static async Task<(AiUsageReport? Report, string? Error)> LoadAsync(AiProviders provider, CancellationToken cancellation)
    {
        var result = await LoadWithPolicyAsync(provider, cancellation);
        return (result.Report, result.Error);
    }

    internal static async Task<LoadResult> LoadWithPolicyAsync(AiProviders provider, CancellationToken cancellation)
    {
        if (provider is not (AiProviders.Claude or AiProviders.Codex or AiProviders.OpenCode or AiProviders.Copilot or AiProviders.DeepSeek or AiProviders.OpenRouter or AiProviders.Cursor))
            return new(null, "ErrorAiUsageLoad");
        int? status = null;
        try
        {
            cancellation.ThrowIfCancellationRequested();
            if (!await Task.Run(NetworkInterface.GetIsNetworkAvailable, cancellation).ConfigureAwait(false))
                return new(null, "ErrorAiUsageLoad");
            if (provider == AiProviders.Cursor)
                return await LoadCursorWithPolicyAsync(cancellation).ConfigureAwait(false);
            var credential = await Task.Run(() => ReadCredential(provider), cancellation).ConfigureAwait(false);
            if (credential is null) return new(null, "ErrorAiUsageSignIn");
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            timeout.CancelAfter(TimeSpan.FromSeconds(20));
            if (provider == AiProviders.Claude && string.IsNullOrWhiteSpace(credential.Token))
            {
                credential = await RefreshClaudeCredentialAsync(credential, null, timeout.Token).ConfigureAwait(false);
                if (credential is null || string.IsNullOrWhiteSpace(credential.Token)) return new(null, "ErrorAiUsageSignIn");
            }
            var response = await SendUsageRequestAsync(provider, credential, timeout.Token).ConfigureAwait(false);
            try
            {
                if (provider == AiProviders.Claude && response.StatusCode == HttpStatusCode.Unauthorized
                    && !string.IsNullOrWhiteSpace(credential.RefreshToken)
                    && await RefreshClaudeCredentialAsync(credential, null, timeout.Token).ConfigureAwait(false) is { Token.Length: > 0 } refreshed)
                {
                    credential = refreshed;
                    response.Dispose();
                    response = await SendUsageRequestAsync(provider, credential, timeout.Token).ConfigureAwait(false);
                }
                status = (int)response.StatusCode;
                if (!response.IsSuccessStatusCode)
                {
                    AppLog.Info($"AI usage ({provider}) unavailable: {status}");
                    return HttpFailure(response.StatusCode, response.Headers.RetryAfter);
                }
                var json = await response.Content.ReadAsStringAsync(timeout.Token).ConfigureAwait(false);
                var (report, error) = provider switch
                {
                    AiProviders.Claude => ParseClaude(json, credential.Plan),
                    AiProviders.Codex => ParseCodex(json),
                    AiProviders.OpenCode => ParseOpenCode(json, credential.Plan),
                    AiProviders.Copilot => ParseCopilot(json, credential.Plan),
                    AiProviders.DeepSeek => ParseDeepSeek(json, credential.Plan),
                    AiProviders.OpenRouter => ParseOpenRouter(json, credential.Plan),
                    _ => (null, "ErrorAiUsageLoad")
                };
                return new(report, error);
            }
            finally { response.Dispose(); }
        }
        catch (Exception ex) when (ex is HttpRequestException or OperationCanceledException or TimeoutException
            or IOException or UnauthorizedAccessException or JsonException or KeyNotFoundException
            or InvalidOperationException or ArgumentException or FormatException or NotSupportedException or SecurityException)
        {
            AppLog.Info($"AI usage ({provider}) unavailable: {status} {ex.GetType().Name}");
            return new(null, "ErrorAiUsageLoad");
        }
    }

    internal static LoadResult HttpFailure(HttpStatusCode status, RetryConditionHeaderValue? retryAfter) => status switch
    {
        HttpStatusCode.Unauthorized => new(null, "ErrorAiUsageSignIn"),
        HttpStatusCode.TooManyRequests => new(null, "ErrorAiUsageRateLimited",
            retryAfter?.Delta ?? (retryAfter?.Date is { } date ? date - DateTimeOffset.UtcNow : null)),
        _ => new(null, "ErrorAiUsageLoad")
    };

    internal static string CredentialPath(AiProviders provider)
    {
        if (provider == AiProviders.Copilot)
            return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "github-copilot", "apps.json");

        var (envVar, folder, file) = provider switch
        {
            AiProviders.Claude => ("CLAUDE_CONFIG_DIR", ".claude", ".credentials.json"),
            AiProviders.Codex => ("CODEX_HOME", ".codex", "auth.json"),
            AiProviders.OpenCode => ("OPENCODE_CONFIG_DIR", Path.Combine(".local", "share", "opencode"), "auth.json"),
            AiProviders.DeepSeek => ("DEEPSEEK_CONFIG_DIR", ".deepseek", "auth.json"),
            AiProviders.OpenRouter => ("OPENROUTER_CONFIG_DIR", ".openrouter", "auth.json"),
            _ => ("OPENAI_CONFIG_DIR", ".config", "auth.json")
        };
        var directory = Environment.GetEnvironmentVariable(envVar);
        if (string.IsNullOrWhiteSpace(directory))
            directory = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), folder);
        return Path.Combine(directory, file);
    }

    internal static DateTime CredentialStamp(AiProviders provider)
    {
        if (provider == AiProviders.Cursor)
            return CursorAuthService.TokenStamp();
        var stamp = DateTime.MinValue;
        if (IsApiKeyProvider(provider))
        {
            var secretStamp = AiSecretStore.GetLastWriteTimeUtc(SecretKey(provider));
            if (secretStamp > stamp) stamp = secretStamp;
        }
        if (provider == AiProviders.Claude && ReadKeyringCredential("Claude Code-credentials", out var claudeKeyringStamp) is not null)
            stamp = claudeKeyringStamp;
        try
        {
            var fileStamp = File.GetLastWriteTimeUtc(CredentialPath(provider));
            if (fileStamp > stamp) stamp = fileStamp;
            if (provider == AiProviders.OpenCode)
            {
                var dbPath = Path.Combine(Path.GetDirectoryName(CredentialPath(provider)) ?? string.Empty, "opencode.db");
                var dbStamp = File.GetLastWriteTimeUtc(dbPath);
                if (dbStamp > stamp) stamp = dbStamp;
            }
            if (provider == AiProviders.Copilot)
            {
                var fallbackStamp = File.GetLastWriteTimeUtc(CopilotConfigAppsPath());
                if (fallbackStamp > stamp) stamp = fallbackStamp;
                // `gh auth login` writes hosts.yml and the Credential Manager entries, never apps.json.
                var ghStamp = GitHubCliStamp(out _);
                if (ghStamp > stamp) stamp = ghStamp;
            }
            return stamp;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException
            or NotSupportedException or SecurityException)
        { return stamp; }
    }

    internal static string? ReadClaudeToken(string? path = null) =>
        ReadCredential(AiProviders.Claude, path ?? CredentialPath(AiProviders.Claude)) is { Token.Length: > 0 } cred ? cred.Token : null;
    internal static string? ReadCodexToken(string? path = null) => ReadCredential(AiProviders.Codex, path ?? CredentialPath(AiProviders.Codex))?.Token;
    internal static string? ReadOpenCodeToken(string? path = null) => ReadCredential(AiProviders.OpenCode, path)?.Token;
    internal static string? ReadCopilotToken(string? path = null) => ReadCredential(AiProviders.Copilot, path)?.Token;
    internal static string? ReadDeepSeekToken(string? path = null) => ReadCredential(AiProviders.DeepSeek, path)?.Token;
    internal static string? ReadOpenRouterToken(string? path = null) => ReadCredential(AiProviders.OpenRouter, path)?.Token;

    internal static async Task<string?> TryRefreshClaudeTokenAsync(
        string path,
        Func<string, CancellationToken, Task<string?>>? exchange = null,
        CancellationToken cancellation = default)
    {
        if (ReadCredential(AiProviders.Claude, path) is not { } credential) return null;
        if (!string.IsNullOrWhiteSpace(credential.Token)) return credential.Token;
        return (await RefreshClaudeCredentialAsync(credential, exchange, cancellation).ConfigureAwait(false))?.Token;
    }

    private static async Task<HttpResponseMessage> SendUsageRequestAsync(AiProviders provider, Credential credential, CancellationToken cancellation)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, provider switch
        {
            AiProviders.Claude => "https://api.anthropic.com/api/oauth/usage",
            AiProviders.Codex => "https://chatgpt.com/backend-api/wham/usage",
            AiProviders.OpenCode => "https://opencode.ai/zen/go/v1/usage",
            AiProviders.Copilot => "https://api.github.com/copilot_internal/user",
            AiProviders.DeepSeek => "https://api.deepseek.com/user/balance",
            AiProviders.OpenRouter => "https://openrouter.ai/api/v1/credits",
            _ => throw new ArgumentOutOfRangeException(nameof(provider))
        });
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", credential.Token);
        if (provider == AiProviders.Claude) request.Headers.Add("anthropic-beta", "oauth-2025-04-20");
        else if (provider == AiProviders.Copilot) request.Headers.TryAddWithoutValidation("Accept", "application/vnd.github+json");
        else if (!string.IsNullOrWhiteSpace(credential.Account)) request.Headers.Add("ChatGPT-Account-Id", credential.Account);
        return await Client.SendAsync(request, cancellation).ConfigureAwait(false);
    }

    private static Credential? ReadCredential(AiProviders provider, string? path = null, bool allowCliProbe = true)
    {
        if (path is null && IsApiKeyProvider(provider) && AiSecretStore.Get(SecretKey(provider)) is { Length: > 0 } savedSecret)
            return new(savedSecret);
        if (provider == AiProviders.OpenCode && path is null)
        {
            foreach (var envKey in new[] { "OPENCODE_GO_API_KEY", "OPENCODE_API_KEY", "OPENCODE_ZEN_API_KEY" })
                if (Environment.GetEnvironmentVariable(envKey) is { Length: > 0 } envToken && !string.IsNullOrWhiteSpace(envToken))
                    return new(envToken.Trim());
        }
        if (provider == AiProviders.DeepSeek && path is null)
        {
            var envToken = Environment.GetEnvironmentVariable("DEEPSEEK_API_KEY")
                ?? Environment.GetEnvironmentVariable("DEEPSEEK_API_KEY", EnvironmentVariableTarget.User)
                ?? Environment.GetEnvironmentVariable("DEEPSEEK_API_KEY", EnvironmentVariableTarget.Machine);
            if (!string.IsNullOrWhiteSpace(envToken))
                return new(envToken.Trim());
        }
        if (provider == AiProviders.OpenRouter && path is null)
        {
            var envToken = Environment.GetEnvironmentVariable("OPENROUTER_API_KEY")
                ?? Environment.GetEnvironmentVariable("OPENROUTER_API_KEY", EnvironmentVariableTarget.User)
                ?? Environment.GetEnvironmentVariable("OPENROUTER_API_KEY", EnvironmentVariableTarget.Machine);
            if (!string.IsNullOrWhiteSpace(envToken))
                return new(envToken.Trim());
        }
        try
        {
            var targetPath = path ?? CredentialPath(provider);
            if (provider == AiProviders.Claude && path is null && !File.Exists(targetPath)
                && string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("CLAUDE_CONFIG_DIR"))
                && ReadKeyringCredential("Claude Code-credentials", out _) is { } claudeKeyringJson
                && ParseClaudeCredential(claudeKeyringJson, null) is { } claudeKeyringCred)
                return claudeKeyringCred;
            if (provider == AiProviders.OpenCode && path is null && !File.Exists(targetPath))
            {
                var legacyPath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".opencode", "auth.json");
                if (File.Exists(legacyPath)) targetPath = legacyPath;
                else if (allowCliProbe && TryReadOpenCodeCliCredential() is { } cliCred) return cliCred;
            }
            if (provider == AiProviders.Copilot && path is null)
            {
                foreach (var appsPath in new[] { targetPath, CopilotConfigAppsPath() })
                    if (TryReadCopilotApps(appsPath) is { } appsCred) return appsCred;
                return TryReadGitHubCliCredential(allowCliProbe, out _) is { } ghToken ? new(ghToken) : null;
            }
            using var stream = new FileStream(targetPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
            using var document = JsonDocument.Parse(stream);
            var root = document.RootElement;
            if (provider == AiProviders.OpenCode) return ParseOpenCodeCredential(root);
            if (provider == AiProviders.Copilot) return ParseCopilotCredential(root);
            if (provider == AiProviders.DeepSeek) return ParseDeepSeekCredential(root);
            if (provider == AiProviders.OpenRouter) return ParseOpenRouterCredential(root);
            if (provider == AiProviders.Claude)
            {
                var fileCred = ParseClaudeCredential(root, targetPath);
                if (fileCred is { Token.Length: > 0 }) return fileCred;
                if (path is null && string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("CLAUDE_CONFIG_DIR"))
                    && ReadKeyringCredential("Claude Code-credentials", out _) is { } fallbackKeyringJson
                    && ParseClaudeCredential(fallbackKeyringJson, null) is { } fallbackKeyringCred
                    && (!string.IsNullOrWhiteSpace(fallbackKeyringCred.Token) || fileCred is null))
                    return fallbackKeyringCred;
                return fileCred;
            }
            var auth = root.GetProperty("tokens");
            var token = auth.GetProperty("access_token").GetString();
            if (string.IsNullOrWhiteSpace(token)) return null;
            return new(token, Text(auth, "account_id"));
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException
            or KeyNotFoundException or InvalidOperationException or FormatException or ArgumentException
            or NotSupportedException or SecurityException)
        { return null; }
    }

    private static Credential? ParseClaudeCredential(string json, string? filePath)
    {
        try
        {
            using var document = JsonDocument.Parse(json);
            return ParseClaudeCredential(document.RootElement, filePath);
        }
        catch (JsonException) { return null; }
    }

    private static Credential? ParseClaudeCredential(JsonElement root, string? filePath)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        var auth = Property(root, "claudeAiOauth") ?? root;
        if (auth.ValueKind != JsonValueKind.Object) return null;
        var token = Text(auth, "accessToken") ?? Text(auth, "access_token");
        var refresh = Text(auth, "refreshToken") ?? Text(auth, "refresh_token");
        var plan = Text(auth, "subscriptionType") ?? Text(auth, "rateLimitTier");
        var expired = Property(auth, "expiresAt") is { ValueKind: JsonValueKind.Number } expiry
            && expiry.TryGetInt64(out var ms)
            && DateTimeOffset.FromUnixTimeMilliseconds(ms) <= DateTimeOffset.UtcNow;
        var usableToken = !string.IsNullOrWhiteSpace(token) && !expired ? token : string.Empty;
        if (usableToken.Length == 0 && string.IsNullOrWhiteSpace(refresh)) return null;
        return new(usableToken, Plan: plan, RefreshToken: string.IsNullOrWhiteSpace(refresh) ? null : refresh, FilePath: filePath);
    }

    private static readonly SemaphoreSlim ClaudeRefreshLock = new(1, 1);

    private static async Task<Credential?> RefreshClaudeCredentialAsync(
        Credential credential,
        Func<string, CancellationToken, Task<string?>>? exchange,
        CancellationToken cancellation)
    {
        if (string.IsNullOrWhiteSpace(credential.RefreshToken)) return null;
        await ClaudeRefreshLock.WaitAsync(cancellation).ConfigureAwait(false);
        try
        {
            if (exchange is null)
            {
                var latest = ReadCredential(AiProviders.Claude, credential.FilePath, allowCliProbe: false);
                if (latest is { Token.Length: > 0 } && !string.Equals(latest.RefreshToken, credential.RefreshToken, StringComparison.Ordinal))
                    return latest;
            }

            string? responseJson;
            if (exchange is not null)
            {
                responseJson = await exchange(credential.RefreshToken, cancellation).ConfigureAwait(false);
            }
            else
            {
                var payload = JsonSerializer.Serialize(new
                {
                    grant_type = "refresh_token",
                    refresh_token = credential.RefreshToken,
                    client_id = ClaudeOAuthClientId
                });
                using var request = new HttpRequestMessage(HttpMethod.Post, "https://api.anthropic.com/v1/oauth/token")
                {
                    Content = new StringContent(payload, Encoding.UTF8, "application/json")
                };
                using var response = await Client.SendAsync(request, cancellation).ConfigureAwait(false);
                if (!response.IsSuccessStatusCode) return null;
                responseJson = await response.Content.ReadAsStringAsync(cancellation).ConfigureAwait(false);
            }
            if (string.IsNullOrWhiteSpace(responseJson)) return null;
            using var document = JsonDocument.Parse(responseJson);
            var root = document.RootElement;
            var newAccess = Text(root, "access_token") ?? Text(root, "accessToken");
            if (string.IsNullOrWhiteSpace(newAccess)) return null;
            var newRefresh = Text(root, "refresh_token") ?? Text(root, "refreshToken") ?? credential.RefreshToken;
            var expiresIn = (Property(root, "expires_in") ?? Property(root, "expiresIn")) is { } exp && Number(exp) is { } sec && sec > 0
                ? sec : 28800;
            var newExpiresAt = DateTimeOffset.UtcNow.AddSeconds(expiresIn).ToUnixTimeMilliseconds();
            if (!string.IsNullOrWhiteSpace(credential.FilePath))
                WriteClaudeCredential(credential.FilePath, newAccess, newRefresh, newExpiresAt);
            else if (exchange is null)
                WriteClaudeKeyringCredential(newAccess, newRefresh, newExpiresAt);
            return credential with { Token = newAccess, RefreshToken = newRefresh };
        }
        catch (Exception ex) when (ex is HttpRequestException or OperationCanceledException or TimeoutException
            or IOException or UnauthorizedAccessException or JsonException)
        {
            return null;
        }
        finally
        {
            ClaudeRefreshLock.Release();
        }
    }

    private static string? UpdateClaudeCredentialJson(string existingJson, string accessToken, string refreshToken, long expiresAt)
    {
        using var existing = JsonDocument.Parse(existingJson);
        if (existing.RootElement.ValueKind != JsonValueKind.Object) return null;
        using var stream = new MemoryStream();
        using (var writer = new Utf8JsonWriter(stream, new JsonWriterOptions { Indented = true }))
        {
            writer.WriteStartObject();
            var wroteOauth = false;
            foreach (var prop in existing.RootElement.EnumerateObject())
            {
                if (prop.Name == "claudeAiOauth" && prop.Value.ValueKind == JsonValueKind.Object)
                {
                    wroteOauth = true;
                    writer.WritePropertyName("claudeAiOauth");
                    writer.WriteStartObject();
                    writer.WriteString("accessToken", accessToken);
                    writer.WriteString("refreshToken", refreshToken);
                    writer.WriteNumber("expiresAt", expiresAt);
                    foreach (var child in prop.Value.EnumerateObject())
                    {
                        if (child.Name is "accessToken" or "access_token" or "refreshToken" or "refresh_token" or "expiresAt")
                            continue;
                        child.WriteTo(writer);
                    }
                    writer.WriteEndObject();
                }
                else prop.WriteTo(writer);
            }
            if (!wroteOauth)
            {
                writer.WritePropertyName("claudeAiOauth");
                writer.WriteStartObject();
                writer.WriteString("accessToken", accessToken);
                writer.WriteString("refreshToken", refreshToken);
                writer.WriteNumber("expiresAt", expiresAt);
                writer.WriteEndObject();
            }
            writer.WriteEndObject();
        }
        return Encoding.UTF8.GetString(stream.GetBuffer(), 0, (int)stream.Length);
    }

    private static void WriteClaudeCredential(string path, string accessToken, string refreshToken, long expiresAt)
    {
        var tmpPath = $"{path}.{Guid.NewGuid():N}.tmp";
        try
        {
            if (!File.Exists(path)) return;
            var updatedJson = UpdateClaudeCredentialJson(File.ReadAllText(path, Encoding.UTF8), accessToken, refreshToken, expiresAt);
            if (updatedJson is null) return;
            File.WriteAllText(tmpPath, updatedJson, Encoding.UTF8);
            File.Move(tmpPath, path, overwrite: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            AppLog.Warning($"Could not persist refreshed credential: {ex.Message}");
        }
        finally
        {
            try { if (File.Exists(tmpPath)) File.Delete(tmpPath); } catch { }
        }
    }

    private static void WriteClaudeKeyringCredential(string accessToken, string refreshToken, long expiresAt)
    {
        const string target = "Claude Code-credentials";
        try
        {
            var existingJson = ReadKeyringCredential(target, out _);
            if (string.IsNullOrWhiteSpace(existingJson)) return;
            var updatedJson = UpdateClaudeCredentialJson(existingJson, accessToken, refreshToken, expiresAt);
            if (string.IsNullOrWhiteSpace(updatedJson)) return;
            var blob = Encoding.UTF8.GetBytes(updatedJson);
            if (blob.Length > (1 << 16)) return;
            var blobPtr = Marshal.AllocHGlobal(blob.Length);
            var targetPtr = Marshal.StringToCoTaskMemUni(target);
            try
            {
                Marshal.Copy(blob, 0, blobPtr, blob.Length);
                var cred = new NativeCredential
                {
                    Type = 1,
                    TargetName = targetPtr,
                    CredentialBlobSize = (uint)blob.Length,
                    CredentialBlob = blobPtr,
                    Persist = 2
                };
                if (!CredWrite(ref cred, 0))
                    AppLog.Warning($"Could not persist refreshed keyring credential: win32={Marshal.GetLastWin32Error()}");
            }
            finally
            {
                Marshal.FreeHGlobal(blobPtr);
                Marshal.FreeCoTaskMem(targetPtr);
            }
        }
        catch (Exception ex) when (ex is JsonException or SecurityException or OverflowException)
        {
            AppLog.Warning($"Could not persist refreshed keyring credential: {ex.Message}");
        }
    }

    private static Credential? ParseOpenCodeCredential(JsonElement root)
    {
        static string? ExtractKey(JsonElement element)
        {
            if (element.ValueKind != JsonValueKind.Object) return null;
            var target = Property(element, "value") ?? element;
            return Text(target, "key") ?? Text(target, "apiKey") ?? Text(target, "access_token") ?? Text(target, "accessToken") ?? Text(target, "token");
        }
        if (root.ValueKind == JsonValueKind.Array)
        {
            string? fallback = null;
            foreach (var item in root.EnumerateArray())
            {
                var id = Text(item, "integrationID") ?? Text(item, "id") ?? Text(item, "provider");
                if (id is null || (!id.Equals("opencode-go", StringComparison.OrdinalIgnoreCase)
                    && !id.Equals("opencode", StringComparison.OrdinalIgnoreCase))) continue;
                if (ExtractKey(item) is not { Length: > 0 } key || string.IsNullOrWhiteSpace(key)) continue;
                if (id.Equals("opencode-go", StringComparison.OrdinalIgnoreCase) && Flag(item, "active")) return new(key);
                fallback ??= key;
            }
            return fallback is null ? null : new(fallback);
        }
        if (root.ValueKind == JsonValueKind.Object)
        {
            foreach (var section in new[] { "opencode-go", "opencode", "zen" })
                if (Property(root, section) is { } child && ExtractKey(child) is { Length: > 0 } key && !string.IsNullOrWhiteSpace(key))
                    return new(key, Plan: Text(child, "plan") ?? Text(root, "plan"));
            if (ExtractKey(root) is { Length: > 0 } flatKey && !string.IsNullOrWhiteSpace(flatKey))
                return new(flatKey, Plan: Text(root, "plan"));
        }
        return null;
    }

    private static Credential? TryReadOpenCodeCliCredential()
    {
        try
        {
            var dbPath = Path.Combine(Path.GetDirectoryName(CredentialPath(AiProviders.OpenCode)) ?? string.Empty, "opencode.db");
            if (!File.Exists(dbPath)) return null;
            using var process = new Process
            {
                StartInfo = HiddenCliStartInfo("opencode", "auth", "export")
            };
            if (!process.Start()) return null;
            process.StandardInput.Close();
            AppLog.Info($"AI usage CLI started: OpenCode pid={process.Id}");
            // Drain stderr asynchronously: RedirectStandardError is enabled but nothing read it,
            // so a CLI writing more than the pipe buffer would deadlock before stdout finished.
            process.BeginErrorReadLine();
            var stdoutTask = process.StandardOutput.ReadToEndAsync();
            if (!process.WaitForExit(5000))
            {
                try { process.Kill(entireProcessTree: true); } catch (InvalidOperationException) { }
                return null;
            }
            if (!stdoutTask.Wait(1000))
            {
                return null;
            }
            var stdout = stdoutTask.Result;
            if (process.ExitCode != 0 || string.IsNullOrWhiteSpace(stdout)) return null;
            using var document = JsonDocument.Parse(stdout);
            return ParseOpenCodeCredential(document.RootElement);
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or IOException or JsonException or InvalidOperationException)
        {
            return null;
        }
    }

    internal static (AiUsageReport? Report, string? Error) ParseClaude(string json, string? plan) => Parse(json, AiProviders.Claude, plan);
    internal static (AiUsageReport? Report, string? Error) ParseCodex(string json) => Parse(json, AiProviders.Codex, null);
    internal static (AiUsageReport? Report, string? Error) ParseOpenCode(string json, string? plan = null) => Parse(json, AiProviders.OpenCode, plan);
    internal static (AiUsageReport? Report, string? Error) ParseCopilot(string json, string? plan = null) => Parse(json, AiProviders.Copilot, plan);
    internal static (AiUsageReport? Report, string? Error) ParseDeepSeek(string json, string? plan = null) => Parse(json, AiProviders.DeepSeek, plan);
    internal static (AiUsageReport? Report, string? Error) ParseOpenRouter(string json, string? plan = null) => Parse(json, AiProviders.OpenRouter, plan);

    private static async Task<LoadResult> LoadCursorWithPolicyAsync(CancellationToken cancellation)
    {
        int? status = null;
        try
        {
            var (access, refresh) = await Task.Run(CursorAuthService.LoadTokens, cancellation).ConfigureAwait(false);
            if (string.IsNullOrWhiteSpace(access) && string.IsNullOrWhiteSpace(refresh))
                return new(null, "ErrorAiUsageSignIn");

            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            timeout.CancelAfter(TimeSpan.FromSeconds(20));

            var refreshed = false;
            if ((string.IsNullOrWhiteSpace(access) || CursorAuthService.IsExpiringSoon(access))
                && !string.IsNullOrWhiteSpace(refresh))
            {
                var refreshResult = await CursorAuthService.RefreshAsync(refresh, Client, timeout.Token).ConfigureAwait(false);
                if (refreshResult.SessionExpired)
                    return new(null, "ErrorAiUsageSignIn");
                if (!string.IsNullOrWhiteSpace(refreshResult.AccessToken))
                {
                    access = refreshResult.AccessToken;
                    refresh = refreshResult.RefreshToken;
                    refreshed = true;
                }
                else if (string.IsNullOrWhiteSpace(access))
                {
                    return new(null, "ErrorAiUsageLoad");
                }
            }

            if (string.IsNullOrWhiteSpace(access))
                return new(null, "ErrorAiUsageSignIn");

            var response = await SendCursorDashboardRequestAsync(access, "GetCurrentPeriodUsage", timeout.Token).ConfigureAwait(false);
            try
            {
                if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
                {
                    if (response.StatusCode == HttpStatusCode.Unauthorized && (refreshed || string.IsNullOrWhiteSpace(refresh)))
                    {
                        // Another concurrent load may already have rotated the token. Only clear a
                        // session when the rejected token is still the one on disk.
                        if (string.Equals(CursorAuthService.LoadTokens().AccessToken, access, StringComparison.Ordinal))
                            CursorAuthService.SignOut();
                        return new(null, "ErrorAiUsageSignIn");
                    }

                    if (!refreshed && !string.IsNullOrWhiteSpace(refresh))
                    {
                        var refreshResult = await CursorAuthService.RefreshAsync(refresh, Client, timeout.Token).ConfigureAwait(false);
                        if (refreshResult.SessionExpired)
                            return new(null, "ErrorAiUsageSignIn");
                        if (string.IsNullOrWhiteSpace(refreshResult.AccessToken))
                            return new(null, "ErrorAiUsageLoad");

                        access = refreshResult.AccessToken;
                        refresh = refreshResult.RefreshToken;
                        refreshed = true;
                        response.Dispose();
                        response = await SendCursorDashboardRequestAsync(access, "GetCurrentPeriodUsage", timeout.Token).ConfigureAwait(false);
                        if (response.StatusCode == HttpStatusCode.Unauthorized)
                        {
                            if (string.Equals(CursorAuthService.LoadTokens().AccessToken, access, StringComparison.Ordinal))
                                CursorAuthService.SignOut();
                            return new(null, "ErrorAiUsageSignIn");
                        }
                    }
                }

                status = (int)response.StatusCode;
                if (!response.IsSuccessStatusCode)
                {
                    AppLog.Info($"AI usage (Cursor) unavailable: {status}");
                    return HttpFailure(response.StatusCode, response.Headers.RetryAfter);
                }

                var usageJson = await response.Content.ReadAsStringAsync(timeout.Token).ConfigureAwait(false);
                string? planJson = null;
                try
                {
                    using var planResponse = await SendCursorDashboardRequestAsync(access, "GetPlanInfo", timeout.Token).ConfigureAwait(false);
                    if (planResponse.IsSuccessStatusCode)
                        planJson = await planResponse.Content.ReadAsStringAsync(timeout.Token).ConfigureAwait(false);
                }
                catch (Exception) when (!cancellation.IsCancellationRequested)
                {
                    // Best-effort secondary endpoint: keep usage even if plan info fails.
                }

                var (report, error) = ParseCursor(usageJson, planJson);
                return new(report, error);
            }
            finally { response.Dispose(); }
        }
        catch (Exception ex) when (ex is HttpRequestException or OperationCanceledException or TimeoutException
            or IOException or UnauthorizedAccessException or JsonException or KeyNotFoundException
            or InvalidOperationException or ArgumentException or FormatException or NotSupportedException or SecurityException)
        {
            AppLog.Info($"AI usage (Cursor) unavailable: {status} {ex.GetType().Name}");
            return new(null, "ErrorAiUsageLoad");
        }
    }

    private static async Task<HttpResponseMessage> SendCursorDashboardRequestAsync(string accessToken, string method, CancellationToken cancellation)
    {
        using var request = new HttpRequestMessage(HttpMethod.Post, $"https://api2.cursor.sh/aiserver.v1.DashboardService/{method}")
        {
            Content = new StringContent("{}", Encoding.UTF8, "application/json")
        };
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", accessToken);
        request.Headers.TryAddWithoutValidation("Connect-Protocol-Version", "1");
        return await Client.SendAsync(request, cancellation).ConfigureAwait(false);
    }

    internal static (AiUsageReport? Report, string? Error) ParseCursor(string usageJson, string? planJson = null)
    {
        if (string.IsNullOrWhiteSpace(usageJson))
            return (null, "ErrorAiUsageLoad");

        try
        {
            using var usageDoc = JsonDocument.Parse(usageJson);
            var root = usageDoc.RootElement;
            if (root.ValueKind != JsonValueKind.Object
                || Property(root, "planUsage") is not { ValueKind: JsonValueKind.Object } planUsage
                || Property(planUsage, "totalPercentUsed") is not { } totalElem
                || Number(totalElem) is not { } totalRaw
                || !double.IsFinite(totalRaw))
            {
                return (null, "ErrorAiUsageLoad");
            }

            string? planName = null;
            string? periodHint = null;
            long? planResetMs = null;
            if (!string.IsNullOrWhiteSpace(planJson))
            {
                try
                {
                    using var planDoc = JsonDocument.Parse(planJson);
                    var pi = Property(planDoc.RootElement, "planInfo") ?? planDoc.RootElement;
                    if (pi.ValueKind == JsonValueKind.Object)
                    {
                        planName = Text(pi, "planName");
                        periodHint = Text(pi, "includedUsagePeriod");
                        planResetMs = ParseMilliseconds(pi, "includedUsageResetsAt") ?? ParseMilliseconds(pi, "billingCycleEnd");
                    }
                }
                catch (JsonException) { /* optional secondary payload */ }
            }

            var startMs = ParseMilliseconds(root, "billingCycleStart");
            var endMs = ParseMilliseconds(root, "billingCycleEnd") ?? planResetMs;
            long? windowSecs = startMs is { } sMs && endMs is { } eMs && eMs > sMs ? (eMs - sMs) / 1000 : null;

            var isWeekly = periodHint == "INCLUDED_USAGE_PERIOD_WEEKLY"
                || (periodHint != "INCLUDED_USAGE_PERIOD_MONTHLY" && windowSecs is >= 5 * 86400 and <= 9 * 86400);
            var labelKey = isWeekly ? "AiUsageWeekly" : "AiUsageMonthly";
            if (windowSecs is null && isWeekly) windowSecs = 7 * 86400L;

            var resetsAt = endMs is { } resetMs ? DateTimeOffset.FromUnixTimeMilliseconds(resetMs) : (DateTimeOffset?)null;
            var totalUsed = Math.Clamp(totalRaw / 100.0, 0.0, 1.0);

            var breakdown = new List<AiUsageShare>();
            var autoUsed = Property(planUsage, "autoPercentUsed") is { } autoElem && Number(autoElem) is { } auVal && double.IsFinite(auVal)
                ? (double?)auVal : null;
            var apiUsed = Property(planUsage, "apiPercentUsed") is { } apiElem && Number(apiElem) is { } apiVal && double.IsFinite(apiVal)
                ? (double?)apiVal : null;
            if (apiUsed is > 0.05 || (autoUsed is { } au && Math.Abs(au - totalRaw) >= 0.5))
            {
                if (autoUsed is { } a) breakdown.Add(new("Auto", Math.Clamp(a / 100.0, 0.0, 1.0)));
                if (apiUsed is > 0) breakdown.Add(new("API", Math.Clamp(apiUsed.Value / 100.0, 0.0, 1.0)));
            }

            var now = DateTimeOffset.UtcNow;
            var report = new AiUsageReport(
                AiProviders.Cursor,
                PlanLabel(planName),
                [new AiUsageWindow(labelKey, totalUsed, resetsAt, windowSecs)],
                now)
            {
                LimitReached = totalUsed >= 1.0,
                Breakdown = breakdown
            };
            return (report, null);
        }
        catch (Exception ex) when (ex is JsonException or KeyNotFoundException or InvalidOperationException
            or FormatException or ArgumentException or OverflowException)
        {
            return (null, "ErrorAiUsageLoad");
        }
    }

    private static long? ParseMilliseconds(JsonElement element, string name)
    {
        if (Property(element, name) is not { } value) return null;
        const long maxUnixMs = 253402300799999L;
        if (value.ValueKind == JsonValueKind.Number)
        {
            if (value.TryGetInt64(out var l)) return l is > 0 and <= maxUnixMs ? l : null;
            if (value.TryGetDouble(out var d) && double.IsFinite(d) && d is > 0 and <= maxUnixMs) return (long)d;
        }
        else if (value.ValueKind == JsonValueKind.String && value.GetString() is { } s)
        {
            if (long.TryParse(s, NumberStyles.Integer, CultureInfo.InvariantCulture, out var l)) return l is > 0 and <= maxUnixMs ? l : null;
            if (double.TryParse(s, NumberStyles.Float, CultureInfo.InvariantCulture, out var d) && double.IsFinite(d) && d is > 0 and <= maxUnixMs) return (long)d;
        }
        return null;
    }

    private static Credential? ParseCopilotCredential(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        foreach (var entry in root.EnumerateObject())
        {
            if (entry.Value.ValueKind == JsonValueKind.Object
                && Text(entry.Value, "oauth_token") is { Length: > 0 } token)
            {
                return new(token);
            }
        }
        return null;
    }

    private const string GhKeyringBase64Prefix = "go-keyring-base64:";
    private const string GhKeyringTargetPrefix = "gh:github.com:";

    /// <summary>Test hook: replaces %APPDATA%\GitHub CLI\hosts.yml.</summary>
    internal static string? GhHostsPathOverride { get; set; }
    /// <summary>Test hook: replaces Credential Manager reads for gh targets.</summary>
    internal static Func<string, (string? Value, DateTime LastWrittenUtc)>? GhKeyringReaderOverride { get; set; }
    /// <summary>Test hook: replaces the `gh auth token` subprocess.</summary>
    internal static Func<string?>? GhCliTokenProbeOverride { get; set; }

    private static string CopilotConfigAppsPath() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".config", "github-copilot", "apps.json");

    private static Credential? TryReadCopilotApps(string appsPath)
    {
        try
        {
            if (!File.Exists(appsPath)) return null;
            using var stream = new FileStream(appsPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
            if (stream.Length == 0) return null;
            using var document = JsonDocument.Parse(stream);
            return ParseCopilotCredential(document.RootElement);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException
            or ArgumentException or NotSupportedException or SecurityException) { return null; }
    }

    internal static string GhHostsPath()
    {
        if (GhHostsPathOverride is { } overridePath) return overridePath;
        var configDir = Environment.GetEnvironmentVariable("GH_CONFIG_DIR");
        if (string.IsNullOrWhiteSpace(configDir))
            configDir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "GitHub CLI");
        return Path.Combine(configDir, "hosts.yml");
    }

    /// <summary>Extracts `user:` and an insecure-storage `oauth_token:` from the github.com block of gh's hosts.yml.</summary>
    internal static (string? User, string? PlainToken) ParseGhHostsYaml(string yaml)
    {
        string? user = null, directToken = null, nestedToken = null;
        var inGitHub = false;
        var childIndent = -1;
        foreach (var rawLine in yaml.Split('\n'))
        {
            var line = rawLine.TrimEnd('\r');
            var content = line.TrimStart();
            if (content.Length == 0 || content.StartsWith('#')) continue;
            var indent = line.Length - content.Length;
            if (indent == 0)
            {
                inGitHub = string.Equals(YamlKey(content), "github.com", StringComparison.OrdinalIgnoreCase);
                childIndent = -1;
                continue;
            }
            if (!inGitHub) continue;
            if (childIndent < 0 || indent < childIndent) childIndent = indent;
            var key = YamlKey(content);
            if (key is null) continue;
            var value = YamlValue(content);
            if (indent == childIndent)
            {
                if (key == "user" && value is not null) user = value;
                else if (key == "oauth_token" && value is not null) directToken = value;
            }
            else if (key == "oauth_token" && value is not null) nestedToken ??= value;
        }
        return (user, directToken ?? nestedToken);
    }

    private static string? YamlKey(string content)
    {
        var colon = content.IndexOf(':');
        if (colon <= 0) return null;
        return content[..colon].Trim().Trim('"', '\'');
    }

    private static string? YamlValue(string content)
    {
        var colon = content.IndexOf(':');
        var value = content[(colon + 1)..].Trim();
        if (value.Length >= 2 && value[0] is '"' or '\'')
        {
            var close = value.IndexOf(value[0], 1);
            value = close > 0 ? value[1..close] : string.Empty;
        }
        else
        {
            var comment = value.IndexOf(" #", StringComparison.Ordinal);
            if (comment >= 0) value = value[..comment].TrimEnd();
        }
        return string.IsNullOrWhiteSpace(value) ? null : value;
    }

    /// <summary>
    /// Decodes a gh Credential Manager blob. go-keyring stores the token as UTF-8 bytes, optionally as
    /// "go-keyring-base64:" + Base64(UTF-8). NUL padding (or a UTF-16 blob read as UTF-8) is stripped.
    /// </summary>
    internal static string? DecodeGhKeyringToken(string? raw)
    {
        if (raw is null) return null;
        var value = raw.Replace("\0", string.Empty, StringComparison.Ordinal).Trim();
        if (value.StartsWith(GhKeyringBase64Prefix, StringComparison.Ordinal))
        {
            try
            {
                value = new UTF8Encoding(false, true).GetString(Convert.FromBase64String(value[GhKeyringBase64Prefix.Length..].Trim()))
                    .Replace("\0", string.Empty, StringComparison.Ordinal).Trim();
            }
            catch (Exception ex) when (ex is FormatException or ArgumentException) { return null; }
        }
        return value.Length == 0 ? null : value;
    }

    private static (string? Value, DateTime LastWrittenUtc) ReadGhKeyring(string target)
    {
        if (GhKeyringReaderOverride is { } reader) return reader(target);
        var value = ReadKeyringCredential(target, out var written);
        return (value, written);
    }

    private static (string? User, string? PlainToken, DateTime Stamp) ReadGhHosts()
    {
        try
        {
            var hostsPath = GhHostsPath();
            if (!File.Exists(hostsPath)) return (null, null, DateTime.MinValue);
            var stamp = File.GetLastWriteTimeUtc(hostsPath);
            using var stream = new FileStream(hostsPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
            if (stream.Length > 1 << 20) return (null, null, stamp);
            using var reader = new StreamReader(stream, Encoding.UTF8);
            var (user, plainToken) = ParseGhHostsYaml(reader.ReadToEnd());
            return (user, plainToken, stamp);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException
            or NotSupportedException or SecurityException) { return (null, null, DateTime.MinValue); }
    }

    private static string[] GhKeyringTargets(string? user) =>
        string.IsNullOrWhiteSpace(user) ? [GhKeyringTargetPrefix] : [GhKeyringTargetPrefix, GhKeyringTargetPrefix + user];

    /// <summary>Latest write time across hosts.yml and gh's Credential Manager entries; no subprocess.</summary>
    private static DateTime GitHubCliStamp(out string? user)
    {
        var (hostsUser, _, stamp) = ReadGhHosts();
        user = hostsUser;
        foreach (var target in GhKeyringTargets(hostsUser))
        {
            var (_, written) = ReadGhKeyring(target);
            if (written > stamp) stamp = written;
        }
        return stamp;
    }

    /// <summary>
    /// Reads the token `gh auth login` stored: Credential Manager first, then a plaintext hosts.yml token,
    /// then (only when allowed, never on the UI thread) `gh auth token`. Never logs the token.
    /// </summary>
    internal static string? TryReadGitHubCliCredential(bool allowCliProbe, out DateTime stamp)
    {
        var (user, plainToken, hostsStamp) = ReadGhHosts();
        stamp = hostsStamp;
        string? token = null;
        foreach (var target in GhKeyringTargets(user))
        {
            var (raw, written) = ReadGhKeyring(target);
            if (written > stamp) stamp = written;
            token ??= DecodeGhKeyringToken(raw);
        }
        if (token is null && !string.IsNullOrWhiteSpace(plainToken)) token = plainToken.Trim();
        if (token is null && allowCliProbe) token = GhCliTokenProbeOverride is { } probe ? probe() : RunGhAuthToken();
        return string.IsNullOrWhiteSpace(token) ? null : token.Trim();
    }

    private static string? RunGhAuthToken()
    {
        try
        {
            using var process = new Process { StartInfo = HiddenCliStartInfo("gh", "auth", "token") };
            if (!process.Start()) return null;
            process.StandardInput.Close();
            AppLog.Info($"AI usage CLI started: GitHub CLI pid={process.Id}");
            var stdoutTask = process.StandardOutput.ReadToEndAsync();
            var stderrTask = process.StandardError.ReadToEndAsync();
            if (!process.WaitForExit(5000))
            {
                try { process.Kill(entireProcessTree: true); } catch (InvalidOperationException) { }
                return null;
            }
            if (!stdoutTask.Wait(1000)) return null;
            _ = stderrTask.Wait(1000);
            if (process.ExitCode != 0) return null;
            var stdout = stdoutTask.Result.Trim();
            return stdout.Length == 0 ? null : stdout;
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or IOException or InvalidOperationException
            or AggregateException or NotSupportedException)
        {
            return null;
        }
    }

    private static Credential? ParseDeepSeekCredential(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        var token = Text(root, "api_key") ?? Text(root, "key") ?? Text(root, "token");
        return string.IsNullOrWhiteSpace(token) ? null : new(token);
    }

    private static Credential? ParseOpenRouterCredential(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        var token = Text(root, "api_key") ?? Text(root, "key") ?? Text(root, "token");
        return string.IsNullOrWhiteSpace(token) ? null : new(token);
    }

    private static (AiUsageReport? Report, string? Error) Parse(string json, AiProviders provider, string? plan)
    {
        try
        {
            using var document = JsonDocument.Parse(json);
            var root = document.RootElement;
            var now = DateTimeOffset.UtcNow;
            var windows = new List<AiUsageWindow>();
            AiUsageReport report;
            if (provider == AiProviders.Claude)
            {
                var severity = new Dictionary<string, string>();
                if (Property(root, "limits") is { ValueKind: JsonValueKind.Array } limits)
                    foreach (var limit in limits.EnumerateArray())
                        if (Text(limit, "group") is { } group && Text(limit, "severity") is { } level) severity.TryAdd(group, level);
                foreach (var property in root.EnumerateObject())
                {
                    if (property.Name != "five_hour" && property.Name != "seven_day"
                        && !property.Name.StartsWith("seven_day_", StringComparison.Ordinal)) continue;
                    var value = property.Value;
                    if (value.ValueKind != JsonValueKind.Object || !value.TryGetProperty("utilization", out var usage)
                        || Percent(usage) is not { } used) continue;
                    var reset = Text(value, "resets_at");
                    var label = property.Name == "five_hour" ? "AiUsageSession" : property.Name == "seven_day"
                        ? "AiUsageWeekly" : "AiUsageWeeklyModel:" + property.Name[10..];
                    windows.Add(new(label, used,
                        DateTimeOffset.TryParse(reset, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var resetsAt) ? resetsAt : null,
                        property.Name == "five_hour" ? 18000 : 604800,
                        property.Name == "five_hour" ? severity.GetValueOrDefault("session")
                            : property.Name == "seven_day" ? severity.GetValueOrDefault("weekly") : null));
                }
                var breakdown = new List<AiUsageShare>();
                if (Property(Property(root, "seven_day_breakdown"), "rows") is { ValueKind: JsonValueKind.Array } rows)
                    foreach (var row in rows.EnumerateArray())
                        if (Text(row, "display_name") is { Length: > 0 } name && Property(row, "percent_of_limit") is { } share
                            && Percent(share) is { } part && Math.Round(part * 100) >= 1) breakdown.Add(new(name, part));
                report = new(provider, PlanLabel(plan), windows, now) { Breakdown = breakdown, Spend = ParseSpend(root) };
            }
            else if (provider == AiProviders.OpenCode)
            {
                plan ??= Text(root, "plan") ?? Text(root, "plan_type") ?? Text(root, "tier") ?? "Go";
                (string[] Keys, string LabelKey, long Seconds)[] specs =
                [
                    (["rolling5h", "rollingUsage", "five_hour", "fiveHour"], "AiUsageSession", 18000L),
                    (["weekly", "weeklyUsage", "seven_day"], "AiUsageWeekly", 604800L),
                    (["monthly", "monthlyUsage", "thirty_day"], "AiUsageMonthly", 2592000L)
                ];
                foreach (var (keys, labelKey, defaultSeconds) in specs)
                {
                    JsonElement? slot = null;
                    foreach (var key in keys)
                        if (Property(root, key) is { ValueKind: JsonValueKind.Object } found) { slot = found; break; }
                    if (slot is not { } value) continue;
                    double? used = null;
                    foreach (var pctKey in new[] { "usagePercent", "used_percent", "utilization", "percent" })
                        if (Property(value, pctKey) is { } pct && Percent(pct) is { } parsedPct) { used = parsedPct; break; }
                    if (used is null
                        && (Property(value, "usageDollars") ?? Property(value, "used")) is { } usedElem && Number(usedElem) is { } usedAmt
                        && (Property(value, "limitDollars") ?? Property(value, "limit")) is { } limitElem && Number(limitElem) is { } limitAmt
                        && double.IsFinite(usedAmt) && double.IsFinite(limitAmt) && limitAmt > 0)
                        used = Math.Clamp(usedAmt / limitAmt, 0.0, 1.0);
                    if (used is not { } finalUsed) continue;
                    DateTimeOffset? resetAt = null;
                    foreach (var absKey in new[] { "resetsAt", "reset_at", "resetTime" })
                    {
                        if (Property(value, absKey) is not { } abs) continue;
                        if (abs.ValueKind == JsonValueKind.String
                            && DateTimeOffset.TryParse(abs.GetString(), CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var isoReset))
                        { resetAt = isoReset; break; }
                        if (UnixReset(abs) is { } unixReset) { resetAt = unixReset; break; }
                    }
                    if (resetAt is null)
                    {
                        foreach (var relKey in new[] { "resetInSec", "reset_after_seconds", "resetInSeconds" })
                            if (Property(value, relKey) is { } rel && Number(rel) is { } after && after is >= 0 and < MaxResetSeconds)
                            { resetAt = now.AddSeconds(after); break; }
                    }
                    windows.Add(new(labelKey, finalUsed, resetAt, defaultSeconds));
                }
                report = new(provider, PlanLabel(plan), windows, now)
                {
                    LimitReached = Flag(root, "limit_reached") || Flag(root, "limitReached")
                        || windows.Any(window => window.Used >= 1.0)
                };
            }
            else if (provider == AiProviders.Copilot)
            {
                plan ??= Text(root, "copilot_plan");
                DateTimeOffset? resetAt = null;
                if (Text(root, "quota_reset_date_utc") is { } resetRaw
                    && DateTimeOffset.TryParse(resetRaw, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var parsedReset))
                {
                    resetAt = parsedReset;
                }

                if (Property(root, "quota_snapshots") is { ValueKind: JsonValueKind.Object } snapshots
                    && Property(snapshots, "premium_interactions") is { ValueKind: JsonValueKind.Object } premium
                    && Property(premium, "percent_remaining") is { } remElem
                    && Number(remElem) is { } percentRemaining
                    && double.IsFinite(percentRemaining))
                {
                    var used = Math.Clamp((100.0 - percentRemaining) / 100.0, 0.0, 1.0);
                    windows.Add(new("AiUsageMonthly", used, resetAt, 2592000L));
                }

                report = new(provider, PlanLabel(plan), windows, now)
                {
                    LimitReached = windows.Any(window => window.Used >= 1.0)
                };
            }
            else if (provider == AiProviders.DeepSeek)
            {
                var isAvailable = Flag(root, "is_available");
                double? totalBalance = null;
                string? currency = null;
                if (Property(root, "balance_infos") is { ValueKind: JsonValueKind.Array } infos)
                {
                    foreach (var info in infos.EnumerateArray())
                    {
                        if (info.ValueKind != JsonValueKind.Object) continue;
                        var totalText = Text(info, "total_balance");
                        if (double.TryParse(totalText, NumberStyles.Float, CultureInfo.InvariantCulture, out var parsedBalance))
                        {
                            totalBalance = parsedBalance;
                            currency = Text(info, "currency") ?? "USD";
                            break;
                        }
                    }
                }

                if (totalBalance is not { } bal)
                    return (null, "ErrorAiUsageLoad");

                plan ??= $"{bal.ToString("0.##", CultureInfo.InvariantCulture)} {currency}";
                windows.Add(new("AiUsageBalance", bal <= 0 ? 1.0 : 0.0, null, null));
                report = new(provider, plan, windows, now)
                {
                    Credits = bal,
                    LimitReached = !isAvailable || bal <= 0
                };
            }
            else if (provider == AiProviders.OpenRouter)
            {
                if (Property(root, "data") is not { ValueKind: JsonValueKind.Object } data
                    || Property(data, "total_credits") is not { } totalElem || Number(totalElem) is not { } totalCredits
                    || Property(data, "total_usage") is not { } usedElem || Number(usedElem) is not { } totalUsage
                    || !double.IsFinite(totalCredits) || !double.IsFinite(totalUsage))
                {
                    return (null, "ErrorAiUsageLoad");
                }

                var remaining = Math.Max(0.0, totalCredits - totalUsage);
                var used = totalCredits > 0 ? Math.Clamp(totalUsage / totalCredits, 0.0, 1.0) : 0.0;
                plan ??= $"${remaining.ToString("0.##", CultureInfo.InvariantCulture)}";
                windows.Add(new("AiUsageCreditsLabel", used, null, null));
                report = new(provider, plan, windows, now)
                {
                    Credits = remaining,
                    LimitReached = totalCredits > 0 && remaining <= 0
                };
            }
            else
            {
                plan = Text(root, "plan_type");
                var limits = root.GetProperty("rate_limit");
                foreach (var name in new[] { "primary_window", "secondary_window" })
                {
                    if (limits.ValueKind != JsonValueKind.Object || !limits.TryGetProperty(name, out var value)
                        || value.ValueKind != JsonValueKind.Object || !value.TryGetProperty("used_percent", out var usage)
                        || Percent(usage) is not { } used) continue;
                    long? seconds = Property(value, "limit_window_seconds") is { } duration && Number(duration) is { } length
                        && length is > 0 and < 1e9 ? (long)length : null;
                    // An unusable absolute reset falls back to the relative one; out-of-range values drop only the reset.
                    var reset = (Property(value, "reset_at") is { } absolute ? UnixReset(absolute) : null)
                        ?? (Property(value, "reset_after_seconds") is { } relative && Number(relative) is { } after
                            && after is >= 0 and < MaxResetSeconds ? (DateTimeOffset?)now.AddSeconds(after) : null);
                    var labelKey = seconds is { } span
                        ? (span >= 2419200 ? "AiUsageMonthly" : span >= 604800 ? "AiUsageWeekly" : "AiUsageSession")
                        : (name == "secondary_window" ? "AiUsageWeekly" : "AiUsageSession");
                    windows.Add(new(labelKey, used, reset, seconds));
                }
                var credits = Property(root, "credits");
                var unlimited = Flag(credits, "unlimited");
                report = new(provider, PlanLabel(plan), windows, now)
                {
                    LimitReached = Flag(limits, "limit_reached") || Property(limits, "allowed") is { ValueKind: JsonValueKind.False },
                    UnlimitedCredits = unlimited,
                    Credits = !unlimited && Flag(credits, "has_credits") && Property(credits, "balance") is { } balance
                        && Number(balance) is { } amount && double.IsFinite(amount) ? amount : null,
                    // Only resets usable right now count; available_count also includes ones that do not apply yet.
                    ResetCredits = Property(Property(root, "rate_limit_reset_credits"), "applicable_available_count") is { } count
                        && Number(count) is { } available && available is > 0 and < 1000 ? (int)available : 0
                };
            }
            return windows.Count == 0 ? (null, "ErrorAiUsageLoad") : (report, null);
        }
        catch (Exception ex) when (ex is JsonException or KeyNotFoundException or InvalidOperationException
            or FormatException or ArgumentException or OverflowException)
        { return (null, "ErrorAiUsageLoad"); }
    }

    /// <summary>Claude pay-as-you-go spend, only when the account has it switched on.</summary>
    private static AiUsageSpend? ParseSpend(JsonElement root)
    {
        var spend = Property(root, "spend");
        if (!Flag(spend, "enabled") && !Flag(Property(root, "extra_usage"), "is_enabled")) return null;
        if (Money(Property(spend, "used")) is not { } used) return null;
        var limit = Money(Property(spend, "limit"));
        var percent = Property(spend, "percent") is { } value ? Percent(value) : null;
        return new(used.Amount, limit?.Amount, used.Currency, percent);
    }

    private static (decimal Amount, string Currency)? Money(JsonElement? value)
    {
        if (value is not { } money || Property(money, "amount_minor") is not { } minor || Number(minor) is not { } amount
            || !double.IsFinite(amount) || Math.Abs(amount) > 1e12) return null;
        var exponent = Property(money, "exponent") is { } digits && Number(digits) is { } places && places is >= 0 and <= 6 ? (int)places : 2;
        var currency = Text(money, "currency") is { Length: 3 } code ? code : "USD";
        return ((decimal)amount / (decimal)Math.Pow(10, exponent), currency);
    }

    private static JsonElement? Property(JsonElement? element, string key) => element is { ValueKind: JsonValueKind.Object } value
        && value.TryGetProperty(key, out var result) && result.ValueKind != JsonValueKind.Null ? result : null;

    private static bool Flag(JsonElement? element, string key) => Property(element, key) is { ValueKind: JsonValueKind.True };

    private static string? Text(JsonElement element, string key) => element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;

    private static double? Number(JsonElement value) => value.ValueKind == JsonValueKind.Number ? value.GetDouble()
        : value.ValueKind == JsonValueKind.String && double.TryParse(value.GetString(), NumberStyles.Float,
            CultureInfo.InvariantCulture, out var number) ? number : null;

    private static DateTimeOffset? UnixReset(JsonElement value)
    {
        if (Number(value) is not { } number || !double.IsFinite(number)) return null;
        var seconds = number > 1e12 ? number / 1000 : number;
        return seconds is > 0 and < MaxResetSeconds ? DateTimeOffset.UnixEpoch.AddSeconds(seconds) : null;
    }

    internal static string? PlanLabel(string? plan)
    {
        if (string.IsNullOrWhiteSpace(plan)) return null;
        var words = plan.Trim().Split('_', StringSplitOptions.RemoveEmptyEntries);
        plan = words.Length > 2 && words[0] == "default" && words[1] == "claude"
            ? words[2] : string.Join(' ', words);
        if (plan.Length == 0) return null;
        plan = plan[..Math.Min(24, plan.Length)];
        return char.ToUpperInvariant(plan[0]) + plan[1..];
    }

    private static double? Percent(JsonElement value)
    {
        if (Number(value) is not { } number) return null;
        return double.IsFinite(number) ? Math.Clamp(number / 100, 0, 1) : 0;
    }

    private static HttpClient CreateClient()
    {
        var client = new HttpClient(new SocketsHttpHandler
        {
            UseCookies = false,
            AllowAutoRedirect = false,
            PooledConnectionLifetime = TimeSpan.FromMinutes(10)
        })
        { Timeout = TimeSpan.FromSeconds(15), MaxResponseContentBufferSize = 1 << 20 };
        client.DefaultRequestHeaders.UserAgent.ParseAdd("OpenTaskbar");
        return client;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NativeCredential
    {
        public uint Flags;
        public uint Type;
        public nint TargetName;
        public nint Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public nint CredentialBlob;
        public uint Persist;
        public uint AttributeCount;
        public nint Attributes;
        public nint TargetAlias;
        public nint UserName;
    }

    [DllImport("advapi32.dll", EntryPoint = "CredReadW", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CredRead(string target, uint type, uint reservedFlag, out nint credentialPtr);

    [DllImport("advapi32.dll", EntryPoint = "CredWriteW", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CredWrite(ref NativeCredential credential, uint flags);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern void CredFree(nint credentialPtr);

    private static string? ReadKeyringCredential(string target, out DateTime lastWrittenUtc)
    {
        lastWrittenUtc = DateTime.MinValue;
        if (!CredRead(target, 1, 0, out var ptr) || ptr == 0) return null;
        try
        {
            var cred = Marshal.PtrToStructure<NativeCredential>(ptr);
            var fileTime = ((long)cred.LastWritten.dwHighDateTime << 32) | (uint)cred.LastWritten.dwLowDateTime;
            if (fileTime > 0) lastWrittenUtc = DateTime.FromFileTimeUtc(fileTime);
            if (cred.CredentialBlob == 0 || cred.CredentialBlobSize is 0 or > (1 << 16)) return null;
            var bytes = new byte[cred.CredentialBlobSize];
            Marshal.Copy(cred.CredentialBlob, bytes, 0, bytes.Length);
            return Encoding.UTF8.GetString(bytes);
        }
        catch (Exception ex) when (ex is ArgumentException or OverflowException or SecurityException) { return null; }
        finally { CredFree(ptr); }
    }
}
