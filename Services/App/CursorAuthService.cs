using System.IO;
using System.Net;
using System.Net.Http;
using System.Runtime.InteropServices;
using System.Security;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace OpenTaskbar.Services;

/// <summary>
/// Manages Cursor browser PKCE sign-in and DPAPI-encrypted OAuth token storage.
/// Never reads local Cursor installation files (%APPDATA%\Cursor) or writes tokens into settings.json.
/// </summary>
public static class CursorAuthService
{
    private const string ClientId = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";
    private const string ApiBase = "https://api2.cursor.sh";
    private const uint CryptProtectUiForbidden = 0x1;
    // Rust `platform/cursor_auth.rs` decrypts with this exact byte sequence.
    internal const string Entropy = "OpenTaskbar.CursorAuth.v1";
    internal const string TokenFileName = "cursor-auth.bin";
    private const int MaxTokenFileBytes = 1 << 16;
    private static readonly byte[] EntropyBytes = Encoding.ASCII.GetBytes(Entropy);

    private static readonly object Sync = new();
    private static readonly SemaphoreSlim RefreshLock = new(1, 1);
    private static readonly HttpClient PollClient = CreateClient();

    internal static string TokenFilePath { get; set; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "OpenTaskbar",
        AiSecretStore.SecretsFolderName,
        TokenFileName);

    public static event Action? SessionChanged;
    internal static event Action? RefreshRequested;

    internal static void RequestRefresh() => RefreshRequested?.Invoke();

    public static bool HasSession
    {
        get
        {
            var (access, refresh) = LoadTokens();
            return !string.IsNullOrWhiteSpace(access) || !string.IsNullOrWhiteSpace(refresh);
        }
    }

    internal static DateTime TokenStamp()
    {
        try
        {
            lock (Sync)
            {
                return File.Exists(TokenFilePath) ? File.GetLastWriteTimeUtc(TokenFilePath) : DateTime.MinValue;
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException
            or NotSupportedException or SecurityException)
        {
            return DateTime.MinValue;
        }
    }

    public static (string? AccessToken, string? RefreshToken) LoadTokens()
    {
        byte[] encrypted;
        try
        {
            lock (Sync)
            {
                if (!File.Exists(TokenFilePath)) return (null, null);
                var length = new FileInfo(TokenFilePath).Length;
                if (length <= 0 || length > MaxTokenFileBytes) return (null, null);
                encrypted = File.ReadAllBytes(TokenFilePath);
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException
            or NotSupportedException or SecurityException)
        {
            return (null, null);
        }

        if (encrypted.Length is 0 or > MaxTokenFileBytes) return (null, null);
        var plain = UnprotectBytes(encrypted);
        if (plain is null || plain.Length == 0) return (null, null);

        string raw;
        try
        {
            raw = Encoding.UTF8.GetString(plain);
        }
        finally
        {
            Array.Clear(plain, 0, plain.Length);
        }
        if (string.IsNullOrWhiteSpace(raw)) return (null, null);
        if (raw.TrimStart().StartsWith('{'))
        {
            try
            {
                using var doc = JsonDocument.Parse(raw);
                if (doc.RootElement.ValueKind != JsonValueKind.Object) return (null, null);
                return (ReadString(doc.RootElement, "accessToken"), ReadString(doc.RootElement, "refreshToken"));
            }
            catch (JsonException)
            {
                return (null, null);
            }
        }

        return (raw.Trim(), null);
    }

    public static void SaveTokens(string accessToken, string? refreshToken)
    {
        if (string.IsNullOrWhiteSpace(accessToken))
            throw new ArgumentException("Access token is required.", nameof(accessToken));

        var obj = new JsonObject { ["accessToken"] = accessToken };
        if (!string.IsNullOrWhiteSpace(refreshToken))
            obj["refreshToken"] = refreshToken;

        var plain = Encoding.UTF8.GetBytes(obj.ToJsonString());
        byte[]? encrypted;
        try
        {
            encrypted = ProtectBytes(plain);
        }
        finally
        {
            Array.Clear(plain, 0, plain.Length);
        }
        if (encrypted is null)
            throw new InvalidOperationException("Failed to encrypt Cursor credentials with Windows DPAPI.");

        lock (Sync)
        {
            var path = TokenFilePath;
            var directory = Path.GetDirectoryName(path);
            if (!string.IsNullOrWhiteSpace(directory))
                Directory.CreateDirectory(directory);

            var tmpPath = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try
            {
                File.WriteAllBytes(tmpPath, encrypted);
                File.Move(tmpPath, path, overwrite: true);
            }
            finally
            {
                if (File.Exists(tmpPath))
                {
                    try { File.Delete(tmpPath); } catch (IOException) { }
                }
            }
        }

        SessionChanged?.Invoke();
    }

    public static void SignOut()
    {
        lock (Sync)
        {
            try
            {
                if (File.Exists(TokenFilePath))
                    File.Delete(TokenFilePath);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException
                or NotSupportedException or SecurityException)
            {
                // Ignore delete races; state will re-sync on next read.
            }
        }

        SessionChanged?.Invoke();
    }

    public static string Base64Url(ReadOnlySpan<byte> bytes) =>
        Convert.ToBase64String(bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_');

    public static string PkceChallenge(string verifier) =>
        Base64Url(SHA256.HashData(Encoding.ASCII.GetBytes(verifier)));

    public static (string Verifier, string Challenge, string Uuid, string LoginUrl) StartLogin()
    {
        var verifier = Base64Url(RandomNumberGenerator.GetBytes(32));
        var challenge = PkceChallenge(verifier);
        var uuid = Guid.NewGuid().ToString("D").ToLowerInvariant();
        var url = $"https://cursor.com/loginDeepControl?challenge={challenge}&uuid={uuid}&mode=login";
        return (verifier, challenge, uuid, url);
    }

    /// <summary>Polls every 2 s for up to 3 minutes until the browser login finishes.</summary>
    public static Task<bool> PollLoginAsync(string uuid, string verifier, CancellationToken ct) =>
        PollLoginAsync(uuid, verifier, PollClient, TimeSpan.FromSeconds(2), TimeSpan.FromMinutes(3), ct);

    internal static async Task<bool> PollLoginAsync(
        string uuid,
        string verifier,
        HttpClient client,
        TimeSpan pollInterval,
        TimeSpan timeout,
        CancellationToken ct)
    {
        var url = $"{ApiBase}/auth/poll?uuid={Uri.EscapeDataString(uuid)}&verifier={Uri.EscapeDataString(verifier)}";
        var deadline = DateTimeOffset.UtcNow.Add(timeout);
        while (DateTimeOffset.UtcNow < deadline)
        {
            await Task.Delay(pollInterval, ct).ConfigureAwait(false);
            try
            {
                using var req = new HttpRequestMessage(HttpMethod.Get, url);
                using var res = await client.SendAsync(req, ct).ConfigureAwait(false);
                if (!res.IsSuccessStatusCode) continue;
                var body = await res.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
                if (string.IsNullOrWhiteSpace(body)) continue;
                using var doc = JsonDocument.Parse(body);
                var access = ReadString(doc.RootElement, "accessToken");
                if (string.IsNullOrEmpty(access)) continue;
                ct.ThrowIfCancellationRequested();
                SaveTokens(access, ReadString(doc.RootElement, "refreshToken"));
                return true;
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested) { throw; }
            catch (OperationCanceledException) { /* single poll request timed out */ }
            catch (JsonException) { /* non-JSON body while waiting */ }
            catch (Exception ex) when (ex is HttpRequestException or IOException) { /* transient network hiccup while polling */ }
        }

        return false;
    }

    public static bool IsExpiringSoon(string accessToken)
    {
        if (string.IsNullOrWhiteSpace(accessToken)) return false;
        var parts = accessToken.Split('.');
        if (parts.Length < 2 || string.IsNullOrWhiteSpace(parts[1])) return false;

        try
        {
            var payloadBytes = DecodeBase64Url(parts[1]);
            using var doc = JsonDocument.Parse(payloadBytes);
            if (doc.RootElement.ValueKind != JsonValueKind.Object
                || !doc.RootElement.TryGetProperty("exp", out var expProp))
                return false;

            double exp;
            if (expProp.ValueKind == JsonValueKind.Number && expProp.TryGetDouble(out var num))
                exp = num;
            else if (expProp.ValueKind == JsonValueKind.String
                && double.TryParse(expProp.GetString(), System.Globalization.NumberStyles.Float,
                    System.Globalization.CultureInfo.InvariantCulture, out var parsed))
                exp = parsed;
            else
                return false;

            if (!double.IsFinite(exp) || exp < -62135596800 || exp > 253402300799) return false;
            return DateTimeOffset.FromUnixTimeSeconds((long)exp) <= DateTimeOffset.UtcNow.AddMinutes(5);
        }
        catch (Exception ex) when (ex is FormatException or JsonException or ArgumentException)
        {
            return false;
        }
    }

    public static async Task<(string? AccessToken, string? RefreshToken, bool SessionExpired)> RefreshAsync(
        string refreshToken,
        HttpClient client,
        CancellationToken ct)
    {
        await RefreshLock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            var (currentAccess, currentRefresh) = LoadTokens();
            if (!string.IsNullOrWhiteSpace(currentAccess)
                && !IsExpiringSoon(currentAccess)
                && !string.Equals(currentRefresh, refreshToken, StringComparison.Ordinal))
            {
                return (currentAccess, currentRefresh, false);
            }

            var payload = JsonSerializer.Serialize(new
            {
                grant_type = "refresh_token",
                client_id = ClientId,
                refresh_token = refreshToken
            });
            using var req = new HttpRequestMessage(HttpMethod.Post, $"{ApiBase}/oauth/token")
            {
                Content = new StringContent(payload, Encoding.UTF8, "application/json")
            };

            using var res = await client.SendAsync(req, ct).ConfigureAwait(false);
            if (res.StatusCode is HttpStatusCode.BadRequest or HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
            {
                SignOut();
                return (null, null, true);
            }

            if (!res.IsSuccessStatusCode)
                return (null, refreshToken, false);

            var body = await res.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
            if (string.IsNullOrWhiteSpace(body))
                return (null, refreshToken, false);

            using var doc = JsonDocument.Parse(body);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
                return (null, refreshToken, false);

            var shouldLogout = root.TryGetProperty("shouldLogout", out var logoutProp)
                && logoutProp.ValueKind == JsonValueKind.True;
            var access = ReadString(root, "access_token");
            if (shouldLogout || string.IsNullOrEmpty(access))
            {
                SignOut();
                return (null, null, true);
            }

            var rotatedRefresh = ReadString(root, "refresh_token");
            var nextRefresh = !string.IsNullOrEmpty(rotatedRefresh) ? rotatedRefresh : refreshToken;
            SaveTokens(access, nextRefresh);
            return (access, nextRefresh, false);
        }
        finally
        {
            RefreshLock.Release();
        }
    }

    private static byte[] DecodeBase64Url(string input)
    {
        var normalized = input.Replace('-', '+').Replace('_', '/');
        var mod = normalized.Length % 4;
        if (mod == 2) normalized += "==";
        else if (mod == 3) normalized += "=";
        else if (mod == 1) throw new FormatException("Invalid Base64Url length.");
        return Convert.FromBase64String(normalized);
    }

    private static string? ReadString(JsonElement element, string name) =>
        element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(name, out var prop)
        && prop.ValueKind == JsonValueKind.String
            ? prop.GetString()
            : null;

    private static HttpClient CreateClient()
    {
        var client = new HttpClient(new SocketsHttpHandler
        {
            UseCookies = false,
            AllowAutoRedirect = false,
            PooledConnectionLifetime = TimeSpan.FromMinutes(10)
        })
        {
            Timeout = TimeSpan.FromSeconds(15),
            MaxResponseContentBufferSize = 1 << 20
        };
        client.DefaultRequestHeaders.UserAgent.ParseAdd("OpenTaskbar");
        return client;
    }

    private static byte[]? ProtectBytes(byte[] plainBytes)
    {
        if (plainBytes.Length == 0) return [];
        var handle = GCHandle.Alloc(plainBytes, GCHandleType.Pinned);
        var entropyHandle = GCHandle.Alloc(EntropyBytes, GCHandleType.Pinned);
        var outBlob = default(DATA_BLOB);
        try
        {
            var inBlob = new DATA_BLOB
            {
                cbData = plainBytes.Length,
                pbData = handle.AddrOfPinnedObject()
            };
            var entropyBlob = new DATA_BLOB
            {
                cbData = EntropyBytes.Length,
                pbData = entropyHandle.AddrOfPinnedObject()
            };
            if (!CryptProtectData(ref inBlob, null, ref entropyBlob, 0, 0, CryptProtectUiForbidden, out outBlob)
                || outBlob.pbData == 0 || outBlob.cbData <= 0)
            {
                return null;
            }

            var result = new byte[outBlob.cbData];
            Marshal.Copy(outBlob.pbData, result, 0, outBlob.cbData);
            return result;
        }
        finally
        {
            if (handle.IsAllocated) handle.Free();
            if (entropyHandle.IsAllocated) entropyHandle.Free();
            if (outBlob.pbData != 0) LocalFree(outBlob.pbData);
        }
    }

    private static byte[]? UnprotectBytes(byte[] protectedBytes)
    {
        if (protectedBytes.Length == 0) return [];
        var handle = GCHandle.Alloc(protectedBytes, GCHandleType.Pinned);
        var entropyHandle = GCHandle.Alloc(EntropyBytes, GCHandleType.Pinned);
        var outBlob = default(DATA_BLOB);
        try
        {
            var inBlob = new DATA_BLOB
            {
                cbData = protectedBytes.Length,
                pbData = handle.AddrOfPinnedObject()
            };
            var entropyBlob = new DATA_BLOB
            {
                cbData = EntropyBytes.Length,
                pbData = entropyHandle.AddrOfPinnedObject()
            };
            var ok = CryptUnprotectData(ref inBlob, 0, ref entropyBlob, 0, 0, CryptProtectUiForbidden, out outBlob);
            // Fail closed: never retry without the application entropy. A blob encrypted by
            // something else that happens to look like our token JSON is not a valid session.
            if (!ok || outBlob.pbData == 0 || outBlob.cbData <= 0)
            {
                return null;
            }

            var result = new byte[outBlob.cbData];
            Marshal.Copy(outBlob.pbData, result, 0, outBlob.cbData);
            return result;
        }
        finally
        {
            if (handle.IsAllocated) handle.Free();
            if (entropyHandle.IsAllocated) entropyHandle.Free();
            if (outBlob.pbData != 0) LocalFree(outBlob.pbData);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct DATA_BLOB
    {
        public int cbData;
        public nint pbData;
    }

    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptProtectData(
        ref DATA_BLOB pDataIn,
        string? szDataDescr,
        ref DATA_BLOB pOptionalEntropy,
        nint pvReserved,
        nint pPromptStruct,
        uint dwFlags,
        out DATA_BLOB pDataOut);

    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptUnprotectData(
        ref DATA_BLOB pDataIn,
        nint ppszDataDescr,
        ref DATA_BLOB pOptionalEntropy,
        nint pvReserved,
        nint pPromptStruct,
        uint dwFlags,
        out DATA_BLOB pDataOut);

    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptUnprotectData(
        ref DATA_BLOB pDataIn,
        nint ppszDataDescr,
        nint pOptionalEntropy,
        nint pvReserved,
        nint pPromptStruct,
        uint dwFlags,
        out DATA_BLOB pDataOut);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern nint LocalFree(nint hMem);
}
