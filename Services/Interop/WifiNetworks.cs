using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Xml.Linq;
using Microsoft.Win32.SafeHandles;

namespace OpenTaskbar;

internal static class WifiNetworks
{
    private const uint ConnectedFlag = 1;
    private const uint HasProfileFlag = 2;
    private const uint UserProfile = 2;
    private const uint Infrastructure = 1;
    private const int ConnectedInterface = 1;
    private const int DisconnectingInterface = 3;
    private const int DisconnectedInterface = 4;
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);

    internal sealed record Network(Guid Adapter, string Name, string Ssid, string Profile, uint Signal,
        bool Secured, bool Connected, bool Connectable, uint Authentication, uint Cipher)
    {
        internal string Security => (Authentication, Cipher) switch
        {
            (1, 0) when !Secured => FjordBarText.Get("OpenNetwork"),
            (4, 2 or 4) => FjordBarText.Get("WpaPersonal"),
            (7, 2 or 4) => FjordBarText.Get("Wpa2Personal"),
            (9, 4) => FjordBarText.Get("Wpa3Personal"),
            _ => FjordBarText.Get("SecuredNetwork")
        };
        internal bool Usable => Connectable && (Profile.Length > 0 || SupportsTemporaryProfile);
        private bool SupportsTemporaryProfile => (Authentication, Cipher) switch
        {
            (1, 0) => !Secured,
            (4 or 7, 2 or 4) or (9, 4) => true,
            _ => false
        };
        internal bool NeedsPassword => Secured && Profile.Length == 0 && Usable;
    }

    internal sealed record Snapshot(bool HasAdapter, IReadOnlyList<Network> Networks);

    internal enum ConnectionResult { Failed, Connected, Remembered, SaveFailed }

    internal static async Task<Snapshot> ReadAsync(bool scan, CancellationToken cancellation)
    {
        Check(WlanOpenHandle(2, 0, out _, out var handle));
        using (handle)
        {
            var adapters = GetAdapters(handle);
            if (scan && adapters.Count > 0)
            {
                var results = adapters.Select(adapter => WlanScan(handle, in adapter.Id, 0, 0, 0)).ToArray();
                if (results.All(result => result != 0)) Check(results[0]);
                await Task.Delay(4000, cancellation).ConfigureAwait(false);
            }
            var networks = new List<Network>();
            Win32Exception? readError = null;
            var adaptersRead = 0;
            foreach (var adapter in adapters)
            {
                cancellation.ThrowIfCancellationRequested();
                try
                {
                    networks.AddRange(GetNetworks(handle, adapter.Id));
                    adaptersRead++;
                }
                catch (Win32Exception error) { readError = error; }
            }
            if (adaptersRead == 0 && readError is not null) throw readError;
            var ordered = networks.OrderByDescending(network => network.Connected)
                .ThenByDescending(network => network.Usable)
                .ThenByDescending(network => network.Signal)
                .ThenBy(network => network.Name, StringComparer.CurrentCultureIgnoreCase)
                .DistinctBy(network => (network.Ssid, network.Secured, network.Authentication, network.Profile))
                .ToArray();
            return new Snapshot(adapters.Count > 0, ordered);
        }
    }

    internal static async Task<ConnectionResult> ConnectAsync(Network network, string password, bool remember)
    {
        Check(WlanOpenHandle(2, 0, out _, out var handle));
        using (handle)
        {
            var hasProfile = network.Profile.Length > 0;
            var parameters = new ConnectionParameters
            {
                Mode = hasProfile ? 0u : 1u,
                Profile = hasProfile ? network.Profile : BuildProfile(network, password),
                BssType = Infrastructure
            };
            Check(WlanConnect(handle, network.Adapter, in parameters, 0));
            for (var attempt = 0; attempt < 30; attempt++)
            {
                await Task.Delay(500).ConfigureAwait(false);
                var state = GetAdapters(handle).Find(adapter => adapter.Id == network.Adapter).State;
                if (state == ConnectedInterface && IsConnected(handle, network))
                {
                    if (hasProfile || !remember) return ConnectionResult.Connected;
                    return StoreProfile(handle, network.Adapter, parameters.Profile)
                        ? ConnectionResult.Remembered : ConnectionResult.SaveFailed;
                }
                if (attempt >= 7 && state is DisconnectingInterface or DisconnectedInterface) break;
            }
            return ConnectionResult.Failed;
        }
    }

    internal static async Task<bool> DisconnectAsync(Network network)
    {
        Check(WlanOpenHandle(2, 0, out _, out var handle));
        using (handle)
        {
            if (!IsConnected(handle, network)) return true;
            Check(WlanDisconnect(handle, network.Adapter, 0));
            for (var attempt = 0; attempt < 20; attempt++)
            {
                await Task.Delay(250).ConfigureAwait(false);
                if (!IsConnected(handle, network)) return true;
            }
            return false;
        }
    }

    private static bool IsConnected(WlanHandle handle, Network network) =>
        GetNetworks(handle, network.Adapter).Any(candidate =>
            candidate.Connected && candidate.Ssid == network.Ssid && candidate.Secured == network.Secured);

    private static bool StoreProfile(WlanHandle handle, Guid adapter, string profile) =>
        WlanSetProfile(handle, in adapter, UserProfile, profile, null, false, 0, out _) == 0;

    internal static string BuildProfile(Network network, string password)
    {
        var authentication = network.Authentication switch
        {
            4 => "WPAPSK", 7 => "WPA2PSK", 9 => "WPA3SAE",
            _ when !network.Secured => "open",
            _ => throw new ArgumentException(FjordBarText.Get("ProfileRequired"))
        };
        var encryption = network.Cipher switch
        {
            0 => "none", 2 => "TKIP", 4 => "AES",
            _ => throw new ArgumentException(FjordBarText.Get("UnsupportedCipher"))
        };
        XNamespace ns = "http://www.microsoft.com/networking/WLAN/profile/v1";
        var security = new XElement(ns + "security",
            new XElement(ns + "authEncryption",
                new XElement(ns + "authentication", authentication),
                new XElement(ns + "encryption", encryption),
                new XElement(ns + "useOneX", false)));
        if (network.Secured)
        {
            var rawKey = authentication != "WPA3SAE" && password.Length == 64 && password.All(Uri.IsHexDigit);
            if (!rawKey && (password.Length is < 8 or > 63 || password.Any(character => character is < ' ' or > '~')))
                throw new ArgumentException(FjordBarText.Get("PasswordLength"));
            security.Add(new XElement(ns + "sharedKey",
                new XElement(ns + "keyType", rawKey ? "networkKey" : "passPhrase"),
                new XElement(ns + "protected", false),
                new XElement(ns + "keyMaterial", password)));
        }
        return new XElement(ns + "WLANProfile",
            new XElement(ns + "name", "Fjord-" + network.Ssid),
            new XElement(ns + "SSIDConfig", new XElement(ns + "SSID", new XElement(ns + "hex", network.Ssid))),
            new XElement(ns + "connectionType", "ESS"),
            new XElement(ns + "connectionMode", "manual"),
            new XElement(ns + "MSM", security)).ToString(SaveOptions.DisableFormatting);
    }

    private static List<Adapter> GetAdapters(WlanHandle handle)
    {
        Check(WlanEnumInterfaces(handle, 0, out var pointer));
        try { return ReadList<Adapter>(pointer).ToList(); }
        finally { WlanFreeMemory(pointer); }
    }

    private static List<Network> GetNetworks(WlanHandle handle, Guid adapter)
    {
        Check(WlanGetAvailableNetworkList(handle, in adapter, 0, 0, out var pointer));
        try
        {
            var networks = new List<Network>();
            foreach (var item in ReadList<AvailableNetwork>(pointer))
            {
                if (item.BssType != Infrastructure || item.Ssid.Length is 0 or > 32) continue;
                var bytes = item.Ssid.Bytes.AsSpan(0, (int)item.Ssid.Length);
                var ssid = Convert.ToHexString(bytes);
                string name;
                try { name = StrictUtf8.GetString(bytes); }
                catch (DecoderFallbackException) { name = ssid; }
                name = new string(name.Select(character => char.IsControl(character) ? '�' : character).ToArray());
                var profile = (item.Flags & HasProfileFlag) != 0 ? item.Profile : string.Empty;
                networks.Add(new Network(adapter, name, ssid, profile, item.Signal, item.Security != 0,
                    (item.Flags & ConnectedFlag) != 0, item.Connectable != 0, item.Authentication, item.Cipher));
            }
            return networks;
        }
        finally { WlanFreeMemory(pointer); }
    }

    private static IEnumerable<T> ReadList<T>(nint pointer) where T : struct
    {
        var count = Marshal.ReadInt32(pointer);
        var size = Marshal.SizeOf<T>();
        for (var index = 0; index < count; index++)
            yield return Marshal.PtrToStructure<T>(pointer + 8 + index * size);
    }

    private static void Check(uint result)
    {
        if (result != 0) throw new Win32Exception((int)result);
    }

    private sealed class WlanHandle : SafeHandleZeroOrMinusOneIsInvalid
    {
        public WlanHandle() : base(true) { }
        protected override bool ReleaseHandle() => WlanCloseHandle(handle, 0) == 0;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct Adapter
    {
        internal Guid Id;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] internal string Description;
        internal int State;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct SsidData
    {
        internal uint Length;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 32)] internal byte[] Bytes;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct AvailableNetwork
    {
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] internal string Profile;
        internal SsidData Ssid;
        internal uint BssType, BssidCount, Connectable, InaccessibleReason, PhyCount;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 8)] internal uint[] PhyTypes;
        internal uint MorePhyTypes, Signal, Security, Authentication, Cipher, Flags, Reserved;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct ConnectionParameters
    {
        internal uint Mode;
        [MarshalAs(UnmanagedType.LPWStr)] internal string Profile;
        internal nint Ssid, DesiredBssidList;
        internal uint BssType, Flags;
    }

    [DllImport("wlanapi.dll")] private static extern uint WlanOpenHandle(uint version, nint reserved, out uint negotiated, out WlanHandle handle);
    [DllImport("wlanapi.dll")] private static extern uint WlanCloseHandle(nint handle, nint reserved);
    [DllImport("wlanapi.dll")] private static extern uint WlanEnumInterfaces(WlanHandle handle, nint reserved, out nint interfaces);
    [DllImport("wlanapi.dll")] private static extern uint WlanScan(WlanHandle handle, in Guid adapter, nint ssid, nint information, nint reserved);
    [DllImport("wlanapi.dll")] private static extern uint WlanGetAvailableNetworkList(WlanHandle handle, in Guid adapter, uint flags, nint reserved, out nint networks);
    [DllImport("wlanapi.dll")] private static extern uint WlanConnect(WlanHandle handle, in Guid adapter, in ConnectionParameters parameters, nint reserved);
    [DllImport("wlanapi.dll")] private static extern uint WlanDisconnect(WlanHandle handle, in Guid adapter, nint reserved);
    [DllImport("wlanapi.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern uint WlanSetProfile(WlanHandle handle, in Guid adapter, uint flags, string profile,
        string? profileSecurity, [MarshalAs(UnmanagedType.Bool)] bool overwrite, nint reserved, out uint reason);
    [DllImport("wlanapi.dll")] private static extern void WlanFreeMemory(nint buffer);
}
