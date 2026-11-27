using System.IO;
using System.Runtime.ExceptionServices;
using System.Text;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

internal static class WirelessBatteryChecks
{
    private static readonly string[] RequiredLocalizationKeys =
    [
        "WidgetBluetoothBattery",
        "WidgetBluetoothBatteryDescription",
        "WirelessBatteryCharging",
        "WirelessBatteryLastReading",
        "WirelessBatteryRefresh",
        "WirelessBatterySpareDock",
        "WirelessBatteryLevelEmpty",
        "WirelessBatteryLevelLow",
        "WirelessBatteryLevelMedium",
        "WirelessBatteryLevelFull",
        "WirelessBatteryRename",
        "WirelessBatteryRenamePrompt",
        "WirelessBatteryHideDevice",
        "WirelessBatteryRestoreHidden",
        "WirelessBatteryCopyDiagnostics",
        "WirelessBatteryDiagnosticsCopied",
        "WirelessBatteryLowToast",
        "WirelessBatteryPollInterval",
        "WirelessBatteryAlertThreshold",
        "WirelessBatteryAlertOff",
        "WirelessBatteryListenOnly",
        "WirelessBatteryListenOnlyOn",
        "WirelessBatteryListenOnlyOff"
    ];

    public static int Run(bool headless = false)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        CheckModelsAndCompatibility(Check);
        CheckSharedHidSnapshotAndZeroOpen(Check);
        CheckBleGattAndStandardHidProviders(Check);
        CheckSteelSeriesProvider(Check);
        CheckCompxAtkVxeProvider(Check);
        CheckLogitechProvider(Check);
        CheckRazerProvider(Check);
        CheckHyperXProvider(Check);
        CheckCorsairProvider(Check);
        CheckAsusRogProvider(Check);
        CheckGamepadHidProvider(Check);
        CheckWirelessKeyboard24gProvider(Check);
        CheckXInputProvider(Check);
        CheckAggregatorCacheOutlierAndIsolation(Check);
        CheckJouleInspiredUpgrades(Check);
        CheckSettingsValidationAndPrivacyDiagnostics(Check);
        CheckAllLocaleCatalogs(Check);
        CheckLiveHardwareScan(Check);
        checks += RunUiChecksOnSta(headless);

        return checks;
    }

    private static void CheckModelsAndCompatibility(Action<bool, string> check)
    {
        check(WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.SteelSeriesHid) == "SteelSeries"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.SteelSeriesGg) == "SteelSeries"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.CompxAtkVxe) == "ATK/VXE"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.LogitechHidPlusPlus) == "HID++"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.LogitechGHub) == "HID++"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.RazerHid) == "Razer"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.XInput) == "XInput"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.Bluetooth) == "Bluetooth"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.BluetoothGatt) == "BLE GATT"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.StandardHid) == "HID"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.HyperXHid) == "HyperX"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.CorsairHid) == "Corsair"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.AsusRogHid) == "ROG"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.GamepadHid) == "Gamepad"
              && WirelessDeviceReading.GetSourceLabel(WirelessBatterySource.Keyboard24gHid) == "2.4G",
            "Every WirelessBatterySource must map to its compact UI badge label.");

        var ts = new DateTimeOffset(2026, 11, 22, 16, 0, 0, TimeSpan.Zero);
        var reading = new WirelessDeviceReading(
            Id: "steelseries:1038:12E0",
            Name: "Arctis Nova Pro Wireless",
            Kind: BluetoothDeviceKind.Headphones,
            BatteryLevel: 75,
            IsConnected: true,
            IsCharging: false,
            IsStale: false,
            Source: WirelessBatterySource.SteelSeriesHid,
            SampledAt: ts,
            SecondaryBatteryLevel: 100,
            VoltageMv: null);

        var info = reading.ToBatteryInfo();
        check(info.Name == "Arctis Nova Pro Wireless"
              && info.Id == "steelseries:1038:12E0"
              && info.IsConnected
              && info.BatteryLevel == 75
              && info.Kind == BluetoothDeviceKind.Headphones
              && !info.IsCharging
              && !info.IsStale
              && info.SourceLabel == "SteelSeries"
              && info.SampledAt == ts
              && info.SecondaryBatteryLevel == 100,
            "WirelessDeviceReading.ToBatteryInfo must preserve all extended metadata.");

        var legacyInfo = new BluetoothDeviceBatteryInfo("Legacy Headset", "ble:1", true, 60);
        check(legacyInfo.Kind == BluetoothDeviceKind.Generic
              && !legacyInfo.IsCharging
              && !legacyInfo.IsStale
              && legacyInfo.SourceLabel is null
              && legacyInfo.LevelOverrideText is null
              && legacyInfo.SecondaryBatteryLevel is null,
            "BluetoothDeviceBatteryInfo must remain backwards-compatible with 4-argument callers.");
    }

    private static void CheckSharedHidSnapshotAndZeroOpen(Action<bool, string> check)
    {
        var enumCalls = 0;
        IReadOnlyList<HidInterfaceDescriptor> snapshot =
        [
            new(@"\\?\hid#vid_1234&pid_5678", 0x1234, 0x5678, 1, 0x0001, 0x0002, 8, 0, 0, "Generic Wired Mouse")
        ];

        var hidProviders = new IWirelessBatteryProvider[]
        {
            new SteelSeriesBatteryProvider(),
            new CompxAtkVxeBatteryProvider(),
            new LogitechBatteryProvider(),
            new RazerBatteryProvider(),
            new HyperXBatteryProvider(),
            new CorsairBatteryProvider(),
            new AsusRogBatteryProvider(),
            new GamepadHidBatteryProvider(),
            new WirelessKeyboard24gProvider(),
            new StandardHidBatteryProvider()
        };

        var aggregator = new WirelessBatteryAggregator(
            bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
            providers: hidProviders,
            clock: () => DateTimeOffset.UtcNow,
            providerTimeout: TimeSpan.FromMilliseconds(300),
            hidEnumerator: () =>
            {
                Interlocked.Increment(ref enumCalls);
                return snapshot;
            });

        var readings = aggregator.PollAsync().GetAwaiter().GetResult();
        check(enumCalls == 1 && readings.Count == 0,
            "WirelessBatteryAggregator must enumerate HID interfaces exactly once per poll and all HID providers must fast-exit with 0 readings when their VID/UsagePage is absent.");
    }

    private static void CheckBleGattAndStandardHidProviders(Action<bool, string> check)
    {
        check(BleGattBatteryProvider.BatteryServiceShortId == 0x180F
              && BleGattBatteryProvider.BatteryLevelCharacteristicShortId == 0x2A19
              && BleGattBatteryProvider.IsBleAssociationEndpointId("BluetoothLE#BluetoothLE00:11:22:33:44:55-aa:bb:cc:dd:ee:ff")
              && !BleGattBatteryProvider.IsBleAssociationEndpointId("Bluetooth#Bluetooth00:11:22")
              && !BleGattBatteryProvider.IsBleAssociationEndpointId(null),
            "BleGattBatteryProvider must recognize BluetoothLE# association endpoints and expose standard 0x180F/0x2A19 UUIDs.");

        var now = DateTimeOffset.UtcNow;
        check(BleGattBatteryProvider.TryParseGattBatteryPayload([84], "ble:1", "Keychron K3 Pro", BluetoothDeviceKind.Keyboard, now, out var bleReading)
              && bleReading is not null
              && bleReading.BatteryLevel == 84
              && bleReading.Kind == BluetoothDeviceKind.Keyboard
              && bleReading.Source == WirelessBatterySource.BluetoothGatt,
            "BleGattBatteryProvider must parse valid 0..100 GATT battery byte.");

        check(!BleGattBatteryProvider.TryParseGattBatteryPayload([], "ble:empty", "Empty", BluetoothDeviceKind.Mouse, now, out _)
              && !BleGattBatteryProvider.TryParseGattBatteryPayload([125], "ble:over", "Over", BluetoothDeviceKind.Mouse, now, out _)
              && !BleGattBatteryProvider.TryParseGattBatteryPayload([90], "ble:wraith", "Wraith W75", BluetoothDeviceKind.Keyboard, now, out _),
            "BleGattBatteryProvider must reject empty payloads, >100% values, and ignored wired Wraith W75.");

        var batteryCap = new HidBatteryValueCap(
            IsFeatureReport: true,
            ReportId: 0x05,
            UsagePage: StandardHidBatteryProvider.UsagePageGenericDeviceControls,
            Usage: StandardHidBatteryProvider.UsageBatteryStrength,
            LinkCollection: 0,
            LogicalMin: 0,
            LogicalMax: 100);

        var genericHidDesc = new HidInterfaceDescriptor(
            @"\\?\hid#vid_093a&pid_2533",
            0x093A,
            0x2533,
            1,
            0x0001,
            0x0002,
            8,
            0,
            4,
            "PixArt 2.4G Wireless Mouse",
            BatteryValueCaps: [batteryCap]);

        var wraithDesc = genericHidDesc with
        {
            VendorId = WirelessBatteryAggregator.WraithVendorId,
            ProductId = WirelessBatteryAggregator.WraithW75ProductId,
            ProductName = "Wraith W75"
        };

        var razerDesc = genericHidDesc with
        {
            VendorId = RazerBatteryProvider.RazerVendorId
        };

        check(StandardHidBatteryProvider.IsMatchingInterface(genericHidDesc)
              && !StandardHidBatteryProvider.IsMatchingInterface(wraithDesc)
              && !StandardHidBatteryProvider.IsMatchingInterface(razerDesc),
            "StandardHidBatteryProvider must match descriptors exposing standard battery value caps while excluding Wraith W75 and dedicated vendor VIDs.");

        check(StandardHidBatteryProvider.NormalizeLogicalValueToPercent(500, 0, 1000) == 50
              && StandardHidBatteryProvider.NormalizeLogicalValueToPercent(75, 0, 100) == 75
              && StandardHidBatteryProvider.NormalizeLogicalValueToPercent(120, 0, 100) == -1,
            "StandardHidBatteryProvider must normalize logical min/max ranges to 0..100%.");

        check(StandardHidBatteryProvider.TryParseDirectByteReport([0x05, 68, 0x01], 0x05, 0, 100, out var stdPct, out var stdChg)
              && stdPct == 68
              && stdChg
              && !StandardHidBatteryProvider.TryParseDirectByteReport([0x06, 68, 0x00], 0x05, 0, 100, out _, out _)
              && !StandardHidBatteryProvider.TryParseDirectByteReport([0x05, 0, 0x00], 0x05, 0, 100, out _, out _),
            "StandardHidBatteryProvider direct report parser must validate report ID and reject sleeping 0% non-charging reports.");
    }

    private static void CheckSteelSeriesProvider(Action<bool, string> check)
    {
        var req64 = SteelSeriesBatteryProvider.BuildBatteryRequest(64, prependZeroReportId: false);
        check(req64.Length == 64 && req64[0] == 0x06 && req64[1] == 0xB0,
            "SteelSeries 64-byte query must start with [0x06, 0xB0].");

        var req65 = SteelSeriesBatteryProvider.BuildBatteryRequest(65, prependZeroReportId: true);
        check(req65.Length == 65 && req65[0] == 0x00 && req65[1] == 0x06 && req65[2] == 0xB0,
            "SteelSeries 65-byte Windows-framed query must start with [0x00, 0x06, 0xB0].");

        // Real Arctis Nova Pro Wireless 64-byte reply: headset=6/8 (75%), dock spare=8/8 (100%), status=0x08 (connected/discharging)
        var liveReply = new byte[64];
        liveReply[0] = 0x06;
        liveReply[1] = 0xB0;
        liveReply[6] = 0x06;
        liveReply[7] = 0x08;
        liveReply[15] = 0x08;

        var now = DateTimeOffset.UtcNow;
        check(SteelSeriesBatteryProvider.TryParseNovaProReply(liveReply, "ss:1", "Arctis Nova Pro Wireless", now, out var parsed)
              && parsed is not null
              && parsed.BatteryLevel == 75
              && parsed.SecondaryBatteryLevel == 100
              && parsed.IsConnected
              && !parsed.IsCharging
              && parsed.Kind == BluetoothDeviceKind.Headphones,
            "SteelSeries Nova Pro reply must decode headset 6/8 -> 75%, spare dock 8/8 -> 100%, and discharging state 0x08.");

        // Windows 0x00-prefixed 65-byte reply while charging (status = 0x02, headset = 4/8 -> 50%)
        var framedCharging = new byte[65];
        framedCharging[0] = 0x00;
        framedCharging[1] = 0x06;
        framedCharging[2] = 0xB0;
        framedCharging[7] = 0x04;
        framedCharging[8] = 0x06;
        framedCharging[16] = 0x02;
        check(SteelSeriesBatteryProvider.TryParseNovaProReply(framedCharging, "ss:2", "Arctis Nova Pro Wireless", now, out var chargingReading)
              && chargingReading is not null
              && chargingReading.BatteryLevel == 50
              && chargingReading.SecondaryBatteryLevel == 75
              && chargingReading.IsConnected
              && chargingReading.IsCharging,
            "SteelSeries parser must unwrap leading 0x00 Report ID framing and detect charging status 0x02.");

        // Headset turned off / out of range (status = 0x01)
        var offlineReply = (byte[])liveReply.Clone();
        offlineReply[15] = 0x01;
        check(SteelSeriesBatteryProvider.TryParseNovaProReply(offlineReply, "ss:3", "Arctis Nova Pro Wireless", now, out var offlineReading)
              && offlineReading is not null
              && !offlineReading.IsConnected
              && !offlineReading.IsCharging,
            "SteelSeries status 0x01 must mark headset as offline/disconnected.");

        // Invalid battery step (> 8) or invalid status must be rejected
        var invalidStep = (byte[])liveReply.Clone();
        invalidStep[6] = 99;
        check(!SteelSeriesBatteryProvider.TryParseNovaProReply(invalidStep, "ss:bad", "Invalid", now, out _),
            "SteelSeries parser must reject out-of-range battery step values (> 8).");

        // Expanded SteelSeries protocols: Nova 7 (3/4 -> 75%), Nova 5 (82%), Arctis Pro Wireless (2/4 -> 50%, spare 4/4 -> 100%), Aerox 3 Wireless Mouse
        byte[] nova7Reply = [0x06, 0xB0, 0x03, 0x03, 0x00, 0x00];
        check(SteelSeriesBatteryProvider.TryParseProtocolReply(
                  SteelSeriesBatteryProvider.SteelSeriesProtocolKind.Nova7AndArctis7Plus,
                  nova7Reply, "ss:nova7", "Arctis Nova 7", now, out var nova7)
              && nova7 is not null && nova7.BatteryLevel == 75 && nova7.IsConnected && !nova7.IsCharging,
            "SteelSeries Nova 7 / Arctis 7+ parser must scale 3/4 -> 75%.");

        byte[] nova5Reply = [0x06, 0xB0, 0x01, 82, 0x00, 0x00];
        check(SteelSeriesBatteryProvider.TryParseProtocolReply(
                  SteelSeriesBatteryProvider.SteelSeriesProtocolKind.Nova5Series,
                  nova5Reply, "ss:nova5", "Arctis Nova 5", now, out var nova5)
              && nova5 is not null && nova5.BatteryLevel == 82 && nova5.IsConnected && nova5.IsCharging,
            "SteelSeries Nova 5 parser must decode direct 0..100% battery and charging state.");

        byte[] arctisProReply = [0x40, 0xAA, 0x02, 0x04];
        check(SteelSeriesBatteryProvider.TryParseProtocolReply(
                  SteelSeriesBatteryProvider.SteelSeriesProtocolKind.ArctisProWireless,
                  arctisProReply, "ss:pro", "Arctis Pro Wireless", now, out var arctisPro)
              && arctisPro is not null && arctisPro.BatteryLevel == 50 && arctisPro.SecondaryBatteryLevel == 100,
            "SteelSeries Arctis Pro Wireless parser must decode headset and spare battery steps.");

        byte[] aeroxReply = [0xD2, (byte)(0x80 | 65), 0x00];
        check(SteelSeriesBatteryProvider.TryParseProtocolReply(
                  SteelSeriesBatteryProvider.SteelSeriesProtocolKind.WirelessMouse,
                  aeroxReply, "ss:aerox", "Aerox 3 Wireless", now, out var aerox)
              && aerox is not null && aerox.BatteryLevel == 65 && aerox.IsCharging && aerox.Kind == BluetoothDeviceKind.Mouse,
            "SteelSeries wireless mouse parser must extract bit-7 charging flag and percentage.");

        // Loopback validation for SteelSeries GG coreProps.json
        check(SteelSeriesBatteryProvider.TryParseCorePropsLoopbackUri(
                  "{\"encryptedAddress\":\"127.0.0.1:50128\",\"address\":\"127.0.0.1:50116\"}", out var ggUri)
              && ggUri is not null
              && ggUri.Host == "127.0.0.1"
              && ggUri.Port == 50116,
            "coreProps.json parser must accept valid 127.0.0.1 loopback address.");
        check(!SteelSeriesBatteryProvider.TryValidateLoopbackEndpoint("localhost:50116", false, out _)
              && !SteelSeriesBatteryProvider.TryValidateLoopbackEndpoint("192.168.1.5:50116", false, out _)
              && !SteelSeriesBatteryProvider.TryValidateLoopbackEndpoint("127.0.0.1:50116/evil", false, out _),
            "SteelSeries GG loopback validator must strictly reject non-127.0.0.1 hosts and path injections.");
    }

    private static void CheckCompxAtkVxeProvider(Action<bool, string> check)
    {
        var req = CompxAtkVxeBatteryProvider.BuildBatteryRequest();
        check(req.Length == 17
              && req[0] == 0x08
              && req[1] == 0x04
              && req[16] == 0x49
              && CompxAtkVxeBatteryProvider.ComputeChecksum(req) == 0x49,
            "Compx/ATK/VXE 17-byte request must be [0x08, 0x04, 0..0, 0x49].");

        // Verify expanded multi-brand vendor IDs (ATK, Compx/VXE/VGN, MCHOSE, Scyrox, WLMouse, Lamzu, Ninjutso, Ajazz/Attack Shark, Darmoshark/Zaopin)
        ushort[] supportedVids =
        [
            CompxAtkVxeBatteryProvider.CompxVendorId,
            CompxAtkVxeBatteryProvider.AtkVendorId,
            CompxAtkVxeBatteryProvider.MchoseVendorId,
            CompxAtkVxeBatteryProvider.ScyroxVendorId,
            CompxAtkVxeBatteryProvider.WlMouseVendorId,
            CompxAtkVxeBatteryProvider.LamzuVendorId,
            CompxAtkVxeBatteryProvider.NinjutsoVendorId,
            CompxAtkVxeBatteryProvider.TelinkAjazzVendorId,
            CompxAtkVxeBatteryProvider.YzwDarmosharkVendorId
        ];
        check(supportedVids.All(CompxAtkVxeBatteryProvider.IsSupportedVendor)
              && !CompxAtkVxeBatteryProvider.IsSupportedVendor(WirelessBatteryAggregator.WraithVendorId),
            "Compx/Nordic provider must support all 9 expanded mouse vendor IDs and never match Wraith 0x2E3C.");

        // Real ATK A9 Ultra 17-byte reply: 08-04-00-00-00-02-64-01-0F-F8-00-00-00-00-00-00-DB
        byte[] liveAtkReply =
        [
            0x08, 0x04, 0x00, 0x00, 0x00, 0x02, 0x64, 0x01,
            0x0F, 0xF8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xDB
        ];
        var now = DateTimeOffset.UtcNow;
        check(CompxAtkVxeBatteryProvider.TryParseBatteryReply(liveAtkReply, "compx:373B:1119", "ATK A9 Ultra", now, out var atkReading)
              && atkReading is not null
              && atkReading.BatteryLevel == 100
              && atkReading.IsCharging
              && atkReading.VoltageMv == 4088
              && atkReading.Kind == BluetoothDeviceKind.Mouse
              && atkReading.Source == WirelessBatterySource.CompxAtkVxe,
            "Compx/ATK parser must decode live ATK A9 Ultra packet (100%, charging, 4088 mV).");

        // 64-byte Nordic/Compx vendor reply (e.g. WLMouse Beast X / Lamzu Maya / Scyrox V8)
        var nordic64Reply = new byte[64];
        liveAtkReply.CopyTo(nordic64Reply, 0);
        check(CompxAtkVxeBatteryProvider.TryParseNordic64BatteryReply(nordic64Reply, "compx:3710:0001", "WLMouse Beast X", now, out var nordicReading)
              && nordicReading is not null
              && nordicReading.BatteryLevel == 100
              && nordicReading.IsCharging,
            "Compx/Nordic 64-byte parser must decode 64-byte vendor battery responses.");

        // Regression test: 10..16-byte replies starting with [0x08, 0x04] (or 11..17-byte with leading 0x00)
        // must never throw ArgumentOutOfRangeException, and unnumbered [0x04, ...] replies must match IsValidNordic64ReplyHeader.
        byte[] shortNordic12 = [0x08, 0x04, 0, 0, 0, 0x02, 77, 0x00, 0x0F, 0x50, 0, 0];
        byte[] framedShortNordic13 = [0x00, 0x08, 0x04, 0, 0, 0, 0x02, 77, 0x00, 0x0F, 0x50, 0, 0];
        byte[] unnumberedNordic10 = [0x04, 0, 0, 0, 0x02, 63, 0x01, 0x0F, 0x80, 0];
        check(CompxAtkVxeBatteryProvider.IsValidNordic64ReplyHeader(shortNordic12)
              && CompxAtkVxeBatteryProvider.TryParseNordic64BatteryReply(shortNordic12, "compx:short12", "Lamzu Maya", now, out var short12Reading)
              && short12Reading is not null && short12Reading.BatteryLevel == 77 && !short12Reading.IsCharging,
            "TryParseNordic64BatteryReply must safely parse 10..16-byte [0x08, 0x04] replies without throwing.");
        check(CompxAtkVxeBatteryProvider.IsValidNordic64ReplyHeader(framedShortNordic13)
              && CompxAtkVxeBatteryProvider.TryParseNordic64BatteryReply(framedShortNordic13, "compx:short13", "Lamzu Maya", now, out var short13Reading)
              && short13Reading is not null && short13Reading.BatteryLevel == 77,
            "TryParseNordic64BatteryReply must safely unwrap leading 0x00 on 11..17-byte replies without throwing.");
        check(CompxAtkVxeBatteryProvider.IsValidNordic64ReplyHeader(unnumberedNordic10)
              && CompxAtkVxeBatteryProvider.TryParseNordic64BatteryReply(unnumberedNordic10, "compx:unnum", "Scyrox V8", now, out var unnumReading)
              && unnumReading is not null && unnumReading.BatteryLevel == 63 && unnumReading.IsCharging,
            "IsValidNordic64ReplyHeader and TryParseNordic64BatteryReply must consistently accept unnumbered 0x04 replies.");

        // Reject wrong header, > 100%, corrupted checksum, or all-zero uninitialized payload
        byte[] badHeader = [0x09, 0x04, 0, 0, 0, 0, 50, 0, 0x0E, 0x00];
        byte[] badPercent = [0x08, 0x04, 0, 0, 0, 0, 150, 0, 0x0E, 0x00];
        byte[] zeroPayload = [0x08, 0x04, 0, 0, 0, 0, 0, 0, 0x00, 0x00];
        var badChecksum = (byte[])liveAtkReply.Clone();
        badChecksum[16] ^= 0xFF;
        check(!CompxAtkVxeBatteryProvider.TryParseBatteryReply(badHeader, "bad1", "Mouse", now, out _)
              && !CompxAtkVxeBatteryProvider.TryParseBatteryReply(badPercent, "bad2", "Mouse", now, out _)
              && !CompxAtkVxeBatteryProvider.TryParseBatteryReply(zeroPayload, "bad3", "Mouse", now, out _)
              && !CompxAtkVxeBatteryProvider.TryParseBatteryReply(badChecksum, "bad4", "Mouse", now, out _),
            "Compx/ATK parser must reject invalid headers, >100% battery values, corrupted checksums, and all-zero sleeping packets.");
    }

    private static void CheckLogitechProvider(Action<bool, string> check)
    {
        var rootReq = LogitechBatteryProvider.BuildRootGetFeatureRequest(deviceIndex: 0x01, featureId: 0x1004);
        check(rootReq.Length == 20 && rootReq[0] == 0x11 && rootReq[1] == 0x01 && rootReq[4] == 0x10 && rootReq[5] == 0x04,
            "HID++ 2.0 IRoot GetFeature request must encode feature ID 0x1004 in big-endian.");

        byte[] rootReply = [0x11, 0x01, 0x00, 0x0A, 0x06, 0x00, 0x00];
        check(LogitechBatteryProvider.TryParseRootFeatureIndex(rootReply, 0x01, out var featIdx) && featIdx == 0x06,
            "HID++ 2.0 IRoot reply must extract resolved feature index.");

        byte[] unifiedReply = [0x11, 0x01, 0x06, 0x1A, 82, 0x04, 0x01];
        check(LogitechBatteryProvider.TryParseUnifiedBatteryReply(unifiedReply, 0x01, 0x06, out var soc, out var charging)
              && soc == 82 && charging,
            "HID++ 2.0 Unified Battery (0x1004) reply must parse state of charge and charging flag.");

        byte[] statusReply = [0x10, 0x02, 0x05, 0x0A, 45, 0x00, 0x00];
        check(LogitechBatteryProvider.TryParseBatteryStatusReply(statusReply, 0x02, 0x05, out var level, out var statusCharging)
              && level == 45 && !statusCharging,
            "HID++ 2.0 Battery Status (0x1000) short reply must parse discharge percentage.");

        byte[] voltageReply = [0x11, 0x01, 0x07, 0x0A, 0x0F, 0xA0, 0x80]; // 4000 mV, charging flag 0x80
        check(LogitechBatteryProvider.TryParseBatteryVoltageReply(voltageReply, 0x01, 0x07, out var vPct, out var vChg, out var mv)
              && mv == 4000 && vChg && vPct is >= 75 and <= 90,
            "HID++ 2.0 Battery Voltage (0x1001) reply must decode millivolts and estimate percentage.");

        byte[] errorPacket = [0x10, 0x01, 0xFF, 0x0A, 0x02, 0x00, 0x00];
        check(LogitechBatteryProvider.IsErrorPacket(errorPacket),
            "HID++ 2.0 error indicator 0xFF must be detected.");

        check(LogitechBatteryProvider.MapHidPlusPlusDeviceType(0) == BluetoothDeviceKind.Keyboard
              && LogitechBatteryProvider.MapHidPlusPlusDeviceType(3) == BluetoothDeviceKind.Mouse
              && LogitechBatteryProvider.MapHidPlusPlusDeviceType(8) == BluetoothDeviceKind.Headphones
              && LogitechBatteryProvider.ClassifyLogitechKind("Logitech PRO X 2 LIGHTSPEED") == BluetoothDeviceKind.Headphones
              && LogitechBatteryProvider.ClassifyLogitechKind("Logitech G915 TKL") == BluetoothDeviceKind.Keyboard
              && LogitechBatteryProvider.ClassifyLogitechKind("PRO X Superlight 2") == BluetoothDeviceKind.Mouse,
            "Logitech HID++ device type and model classifier must distinguish keyboards, headsets, and mice.");

        check(LogitechBatteryProvider.IsValidGHubLoopbackUri(new Uri("ws://127.0.0.1:9010"))
              && !LogitechBatteryProvider.IsValidGHubLoopbackUri(new Uri("ws://localhost:9010"))
              && !LogitechBatteryProvider.IsValidGHubLoopbackUri(new Uri("wss://127.0.0.1:9010"))
              && !LogitechBatteryProvider.IsValidGHubLoopbackUri(new Uri("ws://127.0.0.1:8080")),
            "G HUB WebSocket validator must strictly enforce ws://127.0.0.1:9010.");
    }

    private static void CheckRazerProvider(Action<bool, string> check)
    {
        var req90 = RazerBatteryProvider.BuildCommandReport(RazerBatteryProvider.CommandIdGetBatteryLevel, transactionId: 0x1F, includeReportId: false);
        check(req90.Length == 90
              && req90[1] == 0x1F
              && req90[5] == 0x02
              && req90[6] == 0x07
              && req90[7] == 0x80
              && req90[88] == (0x02 ^ 0x07 ^ 0x80),
            "Razer 90-byte battery request must compute XOR CRC at byte 88 over bytes 2..87.");

        var reply91 = new byte[91];
        reply91[0] = 0x00; // Windows report ID prefix
        reply91[1] = 0x02; // Status: success
        reply91[2] = 0x1F; // Transaction ID
        reply91[6] = 0x02; // Data size
        reply91[7] = 0x07; // Command class
        reply91[8] = 0x80; // Command ID
        reply91[10] = 204; // Arg1: 204 / 255 = 80%
        reply91[89] = RazerBatteryProvider.ComputeXorCrc(reply91);

        check(RazerBatteryProvider.TryParseCommandReply(reply91, 0x80, out var rawVal)
              && rawVal == 204
              && RazerBatteryProvider.ScaleRawByteToPercent(rawVal) == 80,
            "Razer parser must validate status 0x02 and XOR CRC and scale 204/255 -> 80%.");

        reply91[89] ^= 0xFF; // Corrupt CRC
        check(!RazerBatteryProvider.TryParseCommandReply(reply91, 0x80, out _),
            "Razer parser must reject replies with corrupted XOR CRC.");
    }

    private static void CheckHyperXProvider(Action<bool, string> check)
    {
        check(HyperXBatteryProvider.IsSupportedProduct(HyperXBatteryProvider.HpVendorId, 0x05B7)
              && HyperXBatteryProvider.IsSupportedProduct(HyperXBatteryProvider.KingstonVendorId, 0x1718)
              && HyperXBatteryProvider.IsSupportedProduct(HyperXBatteryProvider.HpVendorId, 0x02B8)
              && !HyperXBatteryProvider.IsSupportedProduct(WirelessBatteryAggregator.WraithVendorId, WirelessBatteryAggregator.WraithW75ProductId),
            "HyperX provider must match verified HP/Kingston wireless headsets and mice.");

        var reqHp = HyperXBatteryProvider.BuildBatteryRequest(HyperXBatteryProvider.HyperXProtocolFamily.HpCloudHeadset64, 64);
        check(reqHp.Length == 64 && reqHp[0] == 0x66 && reqHp[1] == 0x89,
            "HyperX HP Cloud 64-byte request must start with [0x66, 0x89].");

        var now = DateTimeOffset.UtcNow;
        byte[] hpReply = [0x66, 0x89, 0x00, 0x00, 78, 0x01, 0x00];
        check(HyperXBatteryProvider.TryParseBatteryReply(
                  HyperXBatteryProvider.HyperXProtocolFamily.HpCloudHeadset64,
                  hpReply, "hyperx:03F0:05B7", "HyperX Cloud III Wireless", BluetoothDeviceKind.Headphones, now, out var cloud3)
              && cloud3 is not null && cloud3.BatteryLevel == 78 && cloud3.IsCharging && cloud3.Source == WirelessBatterySource.HyperXHid,
            "HyperX HP Cloud reply parser must decode 78% and charging flag.");

        byte[] kingstonReply = [0x21, 0xFF, 0x05, 0x00, 0x00, 0x00, 0x00, 64, 0x00];
        check(HyperXBatteryProvider.TryParseBatteryReply(
                  HyperXBatteryProvider.HyperXProtocolFamily.KingstonCloud20,
                  kingstonReply, "hyperx:0951:1718", "HyperX Cloud II Wireless", BluetoothDeviceKind.Headphones, now, out var cloud2)
              && cloud2 is not null && cloud2.BatteryLevel == 64 && !cloud2.IsCharging,
            "HyperX Kingston Cloud 20-byte reply parser must decode battery percentage.");

        byte[] invalidHyperX = [0x66, 0x89, 0x00, 0x00, 140, 0x00];
        check(!HyperXBatteryProvider.TryParseBatteryReply(
                  HyperXBatteryProvider.HyperXProtocolFamily.HpCloudHeadset64,
                  invalidHyperX, "bad", "Bad", BluetoothDeviceKind.Headphones, now, out _),
            "HyperX parser must reject out-of-range (>100%) battery percentages.");
    }

    private static void CheckCorsairProvider(Action<bool, string> check)
    {
        var bragiReq = CorsairBatteryProvider.BuildBragiGetPropertyRequest(
            CorsairBatteryProvider.BragiSubDeviceEndpoint,
            CorsairBatteryProvider.BragiPropertyBatteryLevel,
            64);
        check(bragiReq.Length == 64 && bragiReq[0] == 0x09 && bragiReq[1] == 0x02 && bragiReq[2] == 0x0F && bragiReq[3] == 0x00,
            "Corsair Bragi GetProperty request must target sub-device endpoint 0x09 and property 0x000F.");

        // Permille response: 845 / 1000 -> 85% (or 84%)
        byte[] bragiPermilleReply = [0x09, 0x02, 0x00, 0x48, 0x03, 0x00]; // 0x0348 = 840 -> 84%
        check(CorsairBatteryProvider.TryParseBragiBatteryLevel(bragiPermilleReply, out var corsairPct)
              && corsairPct == 84,
            "Corsair Bragi parser must scale permille 840/1000 -> 84%.");

        byte[] bragiChargeReply = [0x09, 0x02, 0x00, 0x01, 0x00, 0x00];
        check(CorsairBatteryProvider.TryParseBragiChargingStatus(bragiChargeReply, out var corsairChg) && corsairChg,
            "Corsair Bragi charging status parser must detect charging state.");

        var now = DateTimeOffset.UtcNow;
        byte[] legacyReply = [0xC9, 0x64, 0x02, 0x00, 91, 0x00];
        check(CorsairBatteryProvider.TryParseLegacyHeadsetReply(legacyReply, "corsair:1B1C:0A3E", "Corsair Virtuoso", now, out var virtuoso)
              && virtuoso is not null && virtuoso.BatteryLevel == 91 && virtuoso.IsCharging,
            "Corsair legacy headset parser must decode battery percentage and charging status.");

        byte[] badCorsair = [0x09, 0x02, 0x01, 0x48, 0x03, 0x00]; // status != 0x00
        check(!CorsairBatteryProvider.TryParseBragiBatteryLevel(badCorsair, out _),
            "Corsair Bragi parser must reject non-zero error status bytes.");
    }

    private static void CheckAsusRogProvider(Action<bool, string> check)
    {
        var req = AsusRogBatteryProvider.BuildBatteryRequest(64);
        check(req.Length == 64 && req[0] == 0x12 && req[1] == 0x07,
            "ASUS ROG battery request must start with [0x12, 0x07].");

        var now = DateTimeOffset.UtcNow;
        byte[] rogReply = [0x12, 0x07, 0x00, 0x00, 73, 0x01, 0x00];
        check(AsusRogBatteryProvider.TryParseBatteryReply(rogReply, "asus:0B05:1ACE", "ROG Harpe Ace Aim Lab", now, out var rogMouse)
              && rogMouse is not null
              && rogMouse.BatteryLevel == 73
              && rogMouse.IsCharging
              && rogMouse.Kind == BluetoothDeviceKind.Mouse
              && rogMouse.Source == WirelessBatterySource.AsusRogHid,
            "ASUS ROG parser must decode battery percentage, charging flag, and device classification.");

        check(AsusRogBatteryProvider.ClassifyRogKind("ROG Azoth Wireless") == BluetoothDeviceKind.Keyboard
              && AsusRogBatteryProvider.ClassifyRogKind("ROG Delta S Wireless") == BluetoothDeviceKind.Headphones,
            "ASUS ROG classifier must distinguish Azoth keyboards and Delta headsets.");

        byte[] sleepingRog = [0x12, 0x07, 0x00, 0x00, 0, 0x00, 0x00];
        check(!AsusRogBatteryProvider.TryParseBatteryReply(sleepingRog, "asus:sleep", "ROG Mouse", now, out _),
            "ASUS ROG parser must reject all-zero disconnected/sleeping responses.");
    }

    private static void CheckGamepadHidProvider(Action<bool, string> check)
    {
        var now = DateTimeOffset.UtcNow;

        // 1. Sony DualSense USB report (Report ID 0x01, 64 bytes, byte 53 = (state << 4) | step)
        var dsUsb = new byte[64];
        dsUsb[0] = 0x01;
        dsUsb[53] = (0x01 << 4) | 0x08; // charging (0x01), step 8 -> 80%
        check(GamepadHidBatteryProvider.TryParseDualSenseReport(dsUsb, "pad:ds:usb", "DualSense Wireless Controller", now, out var dsUsbReading)
              && dsUsbReading is not null
              && dsUsbReading.BatteryLevel == 80
              && dsUsbReading.IsCharging
              && dsUsbReading.Kind == BluetoothDeviceKind.Gamepad,
            "DualSense USB report (0x01) must parse byte 53 nibble step 8 -> 80% and charging state 0x01.");

        // 2. Sony DualSense Bluetooth report (Report ID 0x31, 78 bytes, byte 54 = (0x02 << 4) | 0x0A -> Full 100%)
        var dsBt = new byte[78];
        dsBt[0] = 0x31;
        dsBt[54] = (0x02 << 4) | 0x0A; // state 0x02 = full
        check(GamepadHidBatteryProvider.TryParseDualSenseReport(dsBt, "pad:ds:bt", "DualSense Edge", now, out var dsBtReading)
              && dsBtReading is not null
              && dsBtReading.BatteryLevel == 100
              && !dsBtReading.IsCharging,
            "DualSense Bluetooth report (0x31) must parse byte 54 full state 0x02 -> 100%.");

        // Reject DualSense thermal/charge error nibble (0x0A) or truncated packet
        dsBt[54] = (0x0A << 4) | 0x05;
        check(!GamepadHidBatteryProvider.TryParseDualSenseReport(dsBt, "pad:ds:err", "DualSense", now, out _)
              && !GamepadHidBatteryProvider.TryParseDualSenseReport(dsUsb.AsSpan(0, 20), "pad:ds:short", "DualSense", now, out _),
            "DualSense parser must reject hardware error states (>= 0x0A) and short packets.");

        // 3. Sony DualShock 4 USB (Report ID 0x01, byte 30) & Bluetooth (Report ID 0x11, byte 32)
        var ds4Usb = new byte[64];
        ds4Usb[0] = 0x01;
        ds4Usb[30] = 0x10 | 0x06; // cable connected (0x10), step 6 -> 60% charging
        check(GamepadHidBatteryProvider.TryParseDualShock4Report(ds4Usb, "pad:ds4:usb", "DUALSHOCK 4", now, out var ds4UsbReading)
              && ds4UsbReading is not null
              && ds4UsbReading.BatteryLevel == 60
              && ds4UsbReading.IsCharging,
            "DualShock 4 USB report (0x01) must parse byte 30 cable flag and 60% level.");

        var ds4Bt = new byte[78];
        ds4Bt[0] = 0x11;
        ds4Bt[32] = 0x09; // wireless discharging, step 9 -> 90%
        check(GamepadHidBatteryProvider.TryParseDualShock4Report(ds4Bt, "pad:ds4:bt", "DUALSHOCK 4", now, out var ds4BtReading)
              && ds4BtReading is not null
              && ds4BtReading.BatteryLevel == 90
              && !ds4BtReading.IsCharging,
            "DualShock 4 Bluetooth report (0x11) must parse byte 32 wireless 90% level.");

        // 4. Nintendo Switch Pro Controller (Report ID 0x30, byte 2 high nibble)
        var swPro = new byte[64];
        swPro[0] = 0x30;
        swPro[2] = (3 << 5) | 0x10; // level 3 (75%) + charging bit 0x10
        check(GamepadHidBatteryProvider.TryParseSwitchProReport(swPro, "pad:sw", "Switch Pro Controller", now, out var swReading)
              && swReading is not null
              && swReading.BatteryLevel == 75
              && swReading.IsCharging,
            "Switch Pro report (0x30) must decode byte 2 level 3 -> 75% and charging flag.");
    }

    private static void CheckWirelessKeyboard24gProvider(Action<bool, string> check)
    {
        var aulaDesc = new HidInterfaceDescriptor(
            @"\\?\hid#vid_258a&pid_010c",
            WirelessKeyboard24gProvider.SinowealthVendorId,
            0x010C,
            1,
            WirelessKeyboard24gProvider.VendorUsagePageFf00,
            0x0001,
            64,
            64,
            0,
            "AULA F75 2.4G");

        var wraithDesc = new HidInterfaceDescriptor(
            @"\\?\hid#vid_2e3c&pid_c368",
            WirelessBatteryAggregator.WraithVendorId,
            WirelessBatteryAggregator.WraithW75ProductId,
            1,
            0xFF00,
            0x0001,
            64,
            64,
            64,
            "Wraith W75");

        check(WirelessKeyboard24gProvider.IsMatchingInterface(aulaDesc)
              && !WirelessKeyboard24gProvider.IsMatchingInterface(wraithDesc)
              && !WirelessKeyboard24gProvider.IsSupportedKeyboard(WirelessBatteryAggregator.WraithVendorId, WirelessBatteryAggregator.WraithW75ProductId),
            "WirelessKeyboard24gProvider must match verified AULA F75 2.4G receiver and strictly block wired Wraith W75 (2E3C:C368).");

        var now = DateTimeOffset.UtcNow;
        byte[] sinowealthReply = [0x83, 0x01, 86, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        check(WirelessKeyboard24gProvider.IsValidBatteryReplyHeader(WirelessKeyboard24gProvider.LongVendorReportLength, sinowealthReply)
              && WirelessKeyboard24gProvider.TryParseBatteryReply(
                  WirelessKeyboard24gProvider.LongVendorReportLength,
                  sinowealthReply,
                  "kbd24g:258A:010C",
                  "AULA F75 Wireless Keyboard",
                  now,
                  out var aulaReading)
              && aulaReading is not null
              && aulaReading.BatteryLevel == 86
              && aulaReading.IsCharging
              && aulaReading.Kind == BluetoothDeviceKind.Keyboard
              && aulaReading.Source == WirelessBatterySource.Keyboard24gHid,
            "WirelessKeyboard24gProvider must parse 64-byte Sinowealth/AULA battery reply.");

        byte[] unnumberedKbdReply = [0x04, 0, 0, 0, 0x02, 54, 0x00, 0x0F, 0x20, 0];
        check(WirelessKeyboard24gProvider.IsValidBatteryReplyHeader(WirelessKeyboard24gProvider.LongVendorReportLength, unnumberedKbdReply)
              && WirelessKeyboard24gProvider.TryParseBatteryReply(
                  WirelessKeyboard24gProvider.LongVendorReportLength,
                  unnumberedKbdReply,
                  "kbd24g:3151:4011",
                  "MonsGeek Wireless Keyboard",
                  now,
                  out var monsReading)
              && monsReading is not null
              && monsReading.BatteryLevel == 54,
            "WirelessKeyboard24gProvider header validator and parser must consistently handle unnumbered 0x04 replies.");
    }

    private static void CheckXInputProvider(Action<bool, string> check)
    {
        var now = DateTimeOffset.UtcNow;
        check(!XInputBatteryProvider.TryBuildSlotReading(0, XInputBatteryProvider.BatteryTypeDisconnected, XInputBatteryProvider.BatteryLevelFull, now, out _)
              && !XInputBatteryProvider.TryBuildSlotReading(0, XInputBatteryProvider.BatteryTypeWired, XInputBatteryProvider.BatteryLevelFull, now, out _)
              && !XInputBatteryProvider.TryBuildSlotReading(0, XInputBatteryProvider.BatteryTypeUnknown, XInputBatteryProvider.BatteryLevelFull, now, out _),
            "XInput provider must skip wired, disconnected, and unknown controllers.");

        check(XInputBatteryProvider.TryBuildSlotReading(0, XInputBatteryProvider.BatteryTypeAlkaline, XInputBatteryProvider.BatteryLevelMedium, now, out var pad)
              && pad is not null
              && pad.Kind == BluetoothDeviceKind.Gamepad
              && pad.LevelOverrideText == UiText.Get("WirelessBatteryLevelMedium")
              && XInputBatteryProvider.GetFallbackEnglishLevelText(XInputBatteryProvider.BatteryLevelEmpty) == "Empty"
              && XInputBatteryProvider.GetFallbackEnglishLevelText(XInputBatteryProvider.BatteryLevelLow) == "Low"
              && XInputBatteryProvider.GetFallbackEnglishLevelText(XInputBatteryProvider.BatteryLevelMedium) == "Medium"
              && XInputBatteryProvider.GetFallbackEnglishLevelText(XInputBatteryProvider.BatteryLevelFull) == "Full",
            "XInput wireless controller must report coarse level text via LevelOverrideText.");
    }

    private static void CheckAggregatorCacheOutlierAndIsolation(Action<bool, string> check)
    {
        check(WirelessBatteryAggregator.IsIgnoredWiredDevice(0x2E3C, 0xC368, "Wraith W75")
              && WirelessBatteryAggregator.IsIgnoredWiredDevice(0x0000, 0x0000, "Wraith W75 Keyboard"),
            "Wraith W75 (2E3C:C368) must be recognized as an ignored wired device.");

        var fakeNow = new DateTimeOffset(2026, 11, 22, 16, 0, 0, TimeSpan.Zero);
        IReadOnlyList<WirelessDeviceReading> currentAtkReadings =
        [
            new("compx:373B:1119", "ATK A9 Ultra", BluetoothDeviceKind.Mouse, 88, true, false, false, WirelessBatterySource.CompxAtkVxe, fakeNow, VoltageMv: 4010),
            new("usb:2E3C:C368", "Wraith W75", BluetoothDeviceKind.Keyboard, 100, true, false, false, WirelessBatterySource.Bluetooth, fakeNow)
        ];

        IReadOnlyList<WirelessDeviceReading> currentSsHidReadings =
        [
            new("steelseries:1038:12E0", "Arctis Nova Pro Wireless", BluetoothDeviceKind.Headphones, 75, true, false, false, WirelessBatterySource.SteelSeriesHid, fakeNow, SecondaryBatteryLevel: 100)
        ];

        IReadOnlyList<WirelessDeviceReading> currentSsGgReadings =
        [
            new("steelseries:gg:arctis", "Arctis Nova Pro Wireless", BluetoothDeviceKind.Headphones, 70, true, false, false, WirelessBatterySource.SteelSeriesGg, fakeNow)
        ];

        var atkProvider = new DelegateBatteryProvider(WirelessBatterySource.CompxAtkVxe, _ => Task.FromResult(currentAtkReadings));
        var ssHidProvider = new DelegateBatteryProvider(WirelessBatterySource.SteelSeriesHid, _ => Task.FromResult(currentSsHidReadings));
        var ssGgProvider = new DelegateBatteryProvider(WirelessBatterySource.SteelSeriesGg, _ => Task.FromResult(currentSsGgReadings));
        var throwingProvider = new DelegateBatteryProvider(WirelessBatterySource.RazerHid, _ => throw new IOException("Simulated HID failure"));

        var aggregator = new WirelessBatteryAggregator(
            bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
            providers: [atkProvider, ssHidProvider, ssGgProvider, throwingProvider],
            clock: () => fakeNow,
            providerTimeout: TimeSpan.FromMilliseconds(300),
            hidEnumerator: () => []);

        // Poll 1: active devices, Wraith W75 filtered out, SteelSeriesHid preferred over SteelSeriesGg, throwing provider isolated.
        var poll1 = aggregator.PollAsync().GetAwaiter().GetResult();
        check(poll1.Count == 2
              && poll1.All(r => !r.Name.Contains("Wraith W75", StringComparison.OrdinalIgnoreCase)),
            "Aggregator must isolate failing providers, deduplicate devices, and filter out wired Wraith W75.");

        var nova1 = poll1.Single(r => r.Name == "Arctis Nova Pro Wireless");
        check(nova1.Source == WirelessBatterySource.SteelSeriesHid
              && nova1.BatteryLevel == 75
              && nova1.SecondaryBatteryLevel == 100
              && !nova1.IsStale,
            "Aggregator deduplication must prefer direct SteelSeriesHid over SteelSeriesGg and keep spare dock battery.");

        // Outlier Guard Poll A (+1 min): ATK A9 Ultra reports a single-sample implausible drop from 88% -> 15% (-73%).
        // Must be rejected on the first sample, keeping 88%!
        fakeNow = fakeNow.AddMinutes(1);
        currentAtkReadings =
        [
            new("compx:373B:1119", "ATK A9 Ultra", BluetoothDeviceKind.Mouse, 15, true, false, false, WirelessBatterySource.CompxAtkVxe, fakeNow)
        ];
        var pollOutlier1 = aggregator.PollAsync().GetAwaiter().GetResult();
        var atkAfterSingleSpike = pollOutlier1.Single(r => r.Name == "ATK A9 Ultra");
        check(atkAfterSingleSpike.BatteryLevel == 88 && !atkAfterSingleSpike.IsStale,
            "Outlier guard must reject a single-sample >=25% battery drop (88% -> 15%) and retain 88%.");

        // Outlier Guard Poll B (+2 min): 2nd consecutive sample also reports 15% -> confirmed, must update to 15%!
        fakeNow = fakeNow.AddMinutes(1);
        var pollOutlier2 = aggregator.PollAsync().GetAwaiter().GetResult();
        var atkAfterConfirmedDrop = pollOutlier2.Single(r => r.Name == "ATK A9 Ultra");
        check(atkAfterConfirmedDrop.BatteryLevel == 15 && !atkAfterConfirmedDrop.IsStale,
            "Outlier guard must accept a >=25% drop when confirmed by 2 consecutive samples.");

        // Restore ATK to 88% before testing sleep retention
        fakeNow = fakeNow.AddMinutes(1);
        currentAtkReadings =
        [
            new("compx:373B:1119", "ATK A9 Ultra", BluetoothDeviceKind.Mouse, 88, true, false, false, WirelessBatterySource.CompxAtkVxe, fakeNow)
        ];
        _ = aggregator.PollAsync().GetAwaiter().GetResult();

        // Poll 2 (+5 min): ATK mouse goes to sleep (returns no reading), headset turns off (IsConnected=false, 0%).
        fakeNow = fakeNow.AddMinutes(5);
        currentAtkReadings = [];
        currentSsHidReadings =
        [
            new("steelseries:1038:12E0", "Arctis Nova Pro Wireless", BluetoothDeviceKind.Headphones, 0, false, false, false, WirelessBatterySource.SteelSeriesHid, fakeNow, SecondaryBatteryLevel: 100)
        ];
        currentSsGgReadings = [];

        var poll2 = aggregator.PollAsync().GetAwaiter().GetResult();
        var atkStale = poll2.Single(r => r.Name == "ATK A9 Ultra");
        var novaStale = poll2.Single(r => r.Name == "Arctis Nova Pro Wireless");
        check(atkStale.BatteryLevel == 88 && atkStale.IsStale && atkStale.IsConnected,
            "Aggregator must retain last real percentage (88%) and mark IsStale=true when wireless mouse sleeps.");
        check(novaStale.BatteryLevel == 75 && novaStale.IsStale && novaStale.SecondaryBatteryLevel == 100,
            "Aggregator must never overwrite a valid cached battery percentage (75%) with 0% when headset turns off.");

        // Poll 3 (+31 min total): stale cache entries older than 30 minutes must expire.
        fakeNow = fakeNow.AddMinutes(26);
        var poll3 = aggregator.PollAsync().GetAwaiter().GetResult();
        check(poll3.Count == 0,
            "Stale cached wireless readings must expire after 30 minutes of inactivity.");

        // Partial reading retention on soft sub-timeout: a provider that collects slot 1 and then waits until
        // the sub-timeout token cancels on slot 2 must still return its accumulated slot 1 reading.
        var partialProvider = new DelegateBatteryProvider(
            WirelessBatterySource.LogitechHidPlusPlus,
            async ct =>
            {
                var collected = new List<WirelessDeviceReading>
                {
                    new("logitech:C547:01", "Logitech PRO X Superlight", BluetoothDeviceKind.Mouse, 91, true, false, false, WirelessBatterySource.LogitechHidPlusPlus, fakeNow)
                };
                try { await Task.Delay(500, ct).ConfigureAwait(false); }
                catch (OperationCanceledException) { }
                return collected;
            });

        var partialAggregator = new WirelessBatteryAggregator(
            bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
            providers: [partialProvider],
            clock: () => fakeNow,
            providerTimeout: TimeSpan.FromMilliseconds(200),
            hidEnumerator: () => []);
        var partialPoll = partialAggregator.PollAsync().GetAwaiter().GetResult();
        check(partialPoll.Count == 1 && partialPoll[0].BatteryLevel == 91,
            "Aggregator soft sub-timeout must preserve partial readings collected before cancellation.");

        // Same-source same-name devices with distinct IDs must not collapse into a single entry.
        var sameNameReadings = new List<WirelessDeviceReading>
        {
            new("bt:001122334455", "DualSense Wireless Controller", BluetoothDeviceKind.Gamepad, 80, true, false, false, WirelessBatterySource.Bluetooth, fakeNow),
            new("bt:AABBCCDDEEFF", "DualSense Wireless Controller", BluetoothDeviceKind.Gamepad, 35, true, false, false, WirelessBatterySource.Bluetooth, fakeNow)
        };
        var dedupSameName = WirelessBatteryAggregator.DeduplicateReadings(sameNameReadings);
        check(dedupSameName.Count == 2
              && dedupSameName.Any(r => r.Id == "bt:001122334455" && r.BatteryLevel == 80)
              && dedupSameName.Any(r => r.Id == "bt:AABBCCDDEEFF" && r.BatteryLevel == 35),
            "DeduplicateReadings must preserve distinct same-name Bluetooth devices with different IDs.");

        var sameNameAggregator = new WirelessBatteryAggregator(
            bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>(sameNameReadings),
            providers: [],
            clock: () => fakeNow);
        var sameNamePoll = sameNameAggregator.PollAsync().GetAwaiter().GetResult();
        check(sameNamePoll.Count == 2
              && sameNamePoll.Any(r => r.Id == "bt:001122334455" && r.BatteryLevel == 80)
              && sameNamePoll.Any(r => r.Id == "bt:AABBCCDDEEFF" && r.BatteryLevel == 35),
            "PollAsync must preserve distinct same-name devices across cache merging.");

        // Concurrent PollAsync calls must be serialized by _pollGate (max in-flight concurrency == 1).
        var inFlight = 0;
        var maxInFlight = 0;
        var serializedProvider = new DelegateBatteryProvider(
            WirelessBatterySource.LogitechHidPlusPlus,
            async ct =>
            {
                var current = Interlocked.Increment(ref inFlight);
                if (current > maxInFlight) Interlocked.Exchange(ref maxInFlight, current);
                await Task.Delay(40, ct).ConfigureAwait(false);
                Interlocked.Decrement(ref inFlight);
                return [];
            });
        var serializedAggregator = new WirelessBatteryAggregator(
            bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
            providers: [serializedProvider],
            clock: () => fakeNow);
        Task.WhenAll(serializedAggregator.PollAsync(), serializedAggregator.PollAsync()).GetAwaiter().GetResult();
        check(maxInFlight == 1, $"Concurrent PollAsync calls must be serialized (maxInFlight={maxInFlight}).");
    }

    private static void CheckSettingsValidationAndPrivacyDiagnostics(Action<bool, string> check)
    {
        var defaultSettings = new DockSettings();
        check(defaultSettings.WirelessBatteryPollSeconds == 30
              && defaultSettings.WirelessBatteryLowThreshold == 0
              && defaultSettings.IgnoredWirelessDevices is null
              && defaultSettings.WirelessDeviceAliases is null
              && DockSettingsStore.Validate(defaultSettings),
            "Default wireless battery settings must be 30s poll, 0 threshold, and valid.");

        check(DockSettingsStore.Validate(defaultSettings with
              {
                  WirelessBatteryPollSeconds = 60,
                  WirelessBatteryLowThreshold = 20,
                  IgnoredWirelessDevices = ["compx:373B:1119"],
                  WirelessDeviceAliases = new Dictionary<string, string> { ["steelseries:1038:12E0"] = "My Nova Pro" }
              }),
            "Valid wireless battery poll interval, threshold, ignored list, and alias map must pass validation.");

        check(!DockSettingsStore.Validate(defaultSettings with { WirelessBatteryPollSeconds = 5 })
              && !DockSettingsStore.Validate(defaultSettings with { WirelessBatteryPollSeconds = 600 })
              && !DockSettingsStore.Validate(defaultSettings with { WirelessBatteryLowThreshold = 15 })
              && !DockSettingsStore.Validate(defaultSettings with { IgnoredWirelessDevices = [""] })
              && !DockSettingsStore.Validate(defaultSettings with
              {
                  WirelessDeviceAliases = new Dictionary<string, string> { ["id"] = "" }
              }),
            "Out-of-range poll intervals, invalid thresholds, and blank ignore/alias entries must fail validation.");

        // Privacy-safe diagnostic summary check: strictly VID/PID/UsagePage/Usage/ReportLengths only, no paths or names!
        IReadOnlyList<HidInterfaceDescriptor> sampleInterfaces =
        [
            new(@"\\?\hid#vid_373b&pid_1119#7&secret_serial_12345&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
                0x373B, 0x1119, 0x0100, 0xFF02, 0x0002, 17, 17, 0, "Alice's Private Mouse"),
            new(@"\\?\hid#vid_2e3c&pid_c368#7&wraith_serial",
                0x2E3C, 0xC368, 0x0100, 0xFF00, 0x0001, 64, 64, 0, "Wraith W75")
        ];
        var diag = WirelessBatteryAggregator.BuildPrivacySafeDiagnosticSummary(sampleInterfaces);
        check(diag.Contains("VID:373B PID:1119 UsagePage:0xFF02 Usage:0x0002 In:17 Out:17 Feat:0", StringComparison.Ordinal)
              && !diag.Contains("secret_serial", StringComparison.OrdinalIgnoreCase)
              && !diag.Contains(@"\\?\", StringComparison.OrdinalIgnoreCase)
              && !diag.Contains("Alice", StringComparison.OrdinalIgnoreCase)
              && !diag.Contains("2E3C", StringComparison.OrdinalIgnoreCase),
            "Privacy-safe diagnostic summary must include only hex VID/PID/UsagePage/ReportLengths and strip all paths, serials, product names, and Wraith W75.");
    }

    private static void CheckAllLocaleCatalogs(Action<bool, string> check)
    {
        var assembly = typeof(LanguageCatalog).Assembly;
        var resources = assembly.GetManifestResourceNames()
            .Where(name => name.StartsWith("OpenTaskbar.Localization.Languages.", StringComparison.Ordinal))
            .OrderBy(name => name, StringComparer.Ordinal)
            .ToArray();

        check(resources.Length == 14, "All 14 locale JSON resources must be embedded.");

        int? expectedKeyCount = null;
        foreach (var resource in resources)
        {
            using var stream = assembly.GetManifestResourceStream(resource)!;
            check(stream.ReadByte() == 0x7B, $"{resource} must start with '{{' and have no UTF-8 BOM.");
            stream.Position = 0;

            using var document = JsonDocument.Parse(stream);
            check(document.RootElement.ValueKind == JsonValueKind.Object, $"{resource} root must be a JSON object.");

            var keyCount = document.RootElement.EnumerateObject().Count();
            expectedKeyCount ??= keyCount;
            check(keyCount == expectedKeyCount.Value,
                $"{resource} key count ({keyCount}) must match all other locales ({expectedKeyCount.Value}).");

            foreach (var key in RequiredLocalizationKeys)
            {
                check(document.RootElement.TryGetProperty(key, out var val)
                      && val.ValueKind == JsonValueKind.String
                      && !string.IsNullOrWhiteSpace(val.GetString()),
                    $"{resource} must provide a non-empty translation for {key}.");
            }
        }
    }

    private static void CheckLiveHardwareScan(Action<bool, string> check)
    {
        var readings = WirelessBatteryAggregator.ReadAllReadingsAsync().GetAwaiter().GetResult();
        check(readings.All(r => !r.Name.Contains("Wraith W75", StringComparison.OrdinalIgnoreCase)
                                && !r.Id.Contains("2E3C", StringComparison.OrdinalIgnoreCase)),
            "Live WirelessBatteryAggregator scan must never include wired Wraith W75 (2E3C:C368).");
    }

    private static void CheckJouleInspiredUpgrades(Action<bool, string> check)
    {
        var fakeNow = new DateTimeOffset(2026, 11, 23, 12, 0, 0, TimeSpan.Zero);

        // 1. Same-source same-name devices with distinct IDs must never collapse.
        var dupNameDevices = new List<WirelessDeviceReading>
        {
            new("bt:001122334455", "DualSense Wireless Controller", BluetoothDeviceKind.Gamepad, 80, true, false, false, WirelessBatterySource.Bluetooth, fakeNow),
            new("bt:AABBCCDDEEFF", "DualSense Wireless Controller", BluetoothDeviceKind.Gamepad, 35, true, false, false, WirelessBatterySource.Bluetooth, fakeNow)
        };
        var dedupedSameName = WirelessBatteryAggregator.DeduplicateReadings(dupNameDevices);
        check(dedupedSameName.Count == 2
              && dedupedSameName.Any(r => r.Id == "bt:001122334455" && r.BatteryLevel == 80)
              && dedupedSameName.Any(r => r.Id == "bt:AABBCCDDEEFF" && r.BatteryLevel == 35),
            "Same-source same-name devices with distinct IDs must stay separated.");

        // 2. Weakest-first ordering (non-charging low battery first, charging devices after).
        var orderAgg = new WirelessBatteryAggregator(
            bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>(
            [
                new("bt:1", "Mouse High", BluetoothDeviceKind.Mouse, 85, true, false, false, WirelessBatterySource.Bluetooth, fakeNow),
                new("bt:2", "Headset Charging", BluetoothDeviceKind.Headphones, 15, true, true, false, WirelessBatterySource.Bluetooth, fakeNow),
                new("bt:3", "Keyboard Low", BluetoothDeviceKind.Keyboard, 20, true, false, false, WirelessBatterySource.Bluetooth, fakeNow)
            ]),
            providers: [],
            clock: () => fakeNow);
        var ordered = orderAgg.PollAsync().GetAwaiter().GetResult();
        check(ordered.Count == 3
              && ordered[0].Name == "Keyboard Low"
              && ordered[1].Name == "Mouse High"
              && ordered[2].Name == "Headset Charging",
            "Aggregator must order non-charging weakest battery first, followed by charging devices.");

        // 3. Listen-Only mode must skip active-write providers while retaining passive ones.
        var activeCalled = false;
        var passiveCalled = false;
        var listenAgg = new WirelessBatteryAggregator(
            bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
            providers:
            [
                new DelegateBatteryProvider(WirelessBatterySource.CompxAtkVxe, _ =>
                {
                    activeCalled = true;
                    return Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]);
                }),
                new DelegateBatteryProvider(WirelessBatterySource.StandardHid, _ =>
                {
                    passiveCalled = true;
                    return Task.FromResult<IReadOnlyList<WirelessDeviceReading>>(
                    [
                        new("std:1", "Passive Mouse", BluetoothDeviceKind.Mouse, 66, true, false, false, WirelessBatterySource.StandardHid, fakeNow)
                    ]);
                })
            ],
            clock: () => fakeNow);
        var listenResult = listenAgg.PollAsync(listenOnly: true).GetAwaiter().GetResult();
        check(!activeCalled && passiveCalled && listenResult.Count == 1 && listenResult[0].BatteryLevel == 66,
            "Listen-Only mode must skip active-write providers and keep passive providers.");

        // 4. Persistent disk cache across aggregator instances + corrupt JSON resilience.
        var tempDir = Path.Combine(Path.GetTempPath(), "OpenTaskbar_WirelessCacheTest_" + Guid.NewGuid().ToString("N"));
        var cachePath = Path.Combine(tempDir, "cache.json");
        try
        {
            var writerAgg = new WirelessBatteryAggregator(
                bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>(
                [
                    new("bt:disk1", "Persisted Mouse", BluetoothDeviceKind.Mouse, 73, true, false, false, WirelessBatterySource.Bluetooth, fakeNow)
                ]),
                providers: [],
                clock: () => fakeNow,
                cacheFilePath: cachePath);
            var firstDiskPoll = writerAgg.PollAsync().GetAwaiter().GetResult();
            check(firstDiskPoll.Count == 1 && File.Exists(cachePath),
                "Persistent disk cache must save changed live readings to disk.");
            var writtenBytes = File.ReadAllBytes(cachePath);
            var writtenText = Encoding.UTF8.GetString(writtenBytes);
            check(writtenBytes.Length >= 3 && writtenBytes[0] == 0xEF && writtenBytes[1] == 0xBB && writtenBytes[2] == 0xBF,
                "The C# wireless cache must keep its UTF-8 BOM (the Rust reader tolerates it).");
            check(writtenText.Contains("\"BatteryLevel\"", StringComparison.Ordinal)
                  && !File.Exists(Path.ChangeExtension(cachePath, ".json.tmp")),
                "The C# cache must write PascalCase keys through its .json.tmp and leave no temp file.");

            var readerAgg = new WirelessBatteryAggregator(
                bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
                providers: [],
                clock: () => fakeNow.AddMinutes(5),
                cacheFilePath: cachePath);
            var restoredPoll = readerAgg.PollAsync().GetAwaiter().GetResult();
            check(restoredPoll.Count == 1 && restoredPoll[0].BatteryLevel == 73 && restoredPoll[0].IsStale,
                "New aggregator instance must restore recent disk-cached readings as stale.");

            File.WriteAllText(cachePath, "{not-valid-json");
            var corruptAgg = new WirelessBatteryAggregator(
                bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
                providers: [],
                clock: () => fakeNow.AddMinutes(5),
                cacheFilePath: cachePath);
            var corruptPoll = corruptAgg.PollAsync().GetAwaiter().GetResult();
            check(corruptPoll.Count == 0,
                "Corrupt disk cache JSON must be ignored gracefully without throwing.");

            // Faz 3: the Rust port writes BOM-less JSON with lowercase wire keys. C# must read it.
            var writtenAt = DateTimeOffset.UtcNow.ToUnixTimeSeconds() - 42;
            var rustCache = "{\"name:rust\":{\"id\":\"hid:1234\",\"name\":\"Rust Mouse\","
                            + "\"level\":73,\"charging\":true,\"source\":0,"
                            + "\"age_secs\":42,\"written_at\":" + writtenAt.ToString(System.Globalization.CultureInfo.InvariantCulture) + "}}";
            File.WriteAllBytes(cachePath, Encoding.UTF8.GetBytes(rustCache));
            var rustReaderAgg = new WirelessBatteryAggregator(
                bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
                providers: [],
                clock: () => fakeNow,
                cacheFilePath: cachePath);
            var rustPoll = rustReaderAgg.PollAsync().GetAwaiter().GetResult();
            check(rustPoll.Count == 1 && rustPoll[0].BatteryLevel == 73 && rustPoll[0].IsStale
                  && rustPoll[0].Source == WirelessBatterySource.Bluetooth,
                "C# must restore Rust's BOM-less disk cache as a stale reading with its level and source.");

            // A stale Rust written_at timestamp must be rejected even if age_secs claims freshness.
            var staleWrittenAt = DateTimeOffset.UtcNow.ToUnixTimeSeconds() - 3600;
            var staleRustCache = "{\"name:old\":{\"id\":\"hid:9999\",\"name\":\"Old Mouse\","
                                 + "\"level\":55,\"charging\":false,\"source\":0,"
                                 + "\"age_secs\":0,\"written_at\":" + staleWrittenAt.ToString(System.Globalization.CultureInfo.InvariantCulture) + "}}";
            File.WriteAllBytes(cachePath, Encoding.UTF8.GetBytes(staleRustCache));
            var staleReaderAgg = new WirelessBatteryAggregator(
                bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
                providers: [],
                clock: () => fakeNow,
                cacheFilePath: cachePath);
            check(staleReaderAgg.PollAsync().GetAwaiter().GetResult().Count == 0,
                "Rust written_at must override a fresh-looking age_secs when deciding cache staleness.");
        }
        finally
        {
            try { if (Directory.Exists(tempDir)) Directory.Delete(tempDir, recursive: true); } catch { }
        }

        // 5. Full charge highlight helper.
        check(BluetoothBatteryWidget.IsFullChargeHighlight(100, true)
              && !BluetoothBatteryWidget.IsFullChargeHighlight(100, false)
              && !BluetoothBatteryWidget.IsFullChargeHighlight(99, true),
            "IsFullChargeHighlight must only activate at 100% while actively charging.");

        // 6. ATK 1K PAW reply parsing + expanded open-protocol VIDs/PIDs.
        var atk1kReply = new byte[65];
        atk1kReply[0] = 0x00;
        atk1kReply[1] = 0x82;
        atk1kReply[2] = 0x08;
        atk1kReply[3] = 0x02;
        atk1kReply[4] = 0x00;
        atk1kReply[5] = 0xFF;
        atk1kReply[6] = 0x03;
        atk1kReply[9] = 0x80 | 27; // 27%, charging bit set
        check(CompxAtkVxeBatteryProvider.TryParseAtk1kReply(atk1kReply, "atk1k", "ATK 1K PAW", fakeNow, out var atk1kReading)
              && atk1kReading is { BatteryLevel: 27, IsCharging: true },
            "TryParseAtk1kReply must decode 7-bit percentage and high-bit charging flag.");

        check(CompxAtkVxeBatteryProvider.IsSupportedVendor(CompxAtkVxeBatteryProvider.PulsarLamzu8kVendorId)
              && CompxAtkVxeBatteryProvider.IsSupportedVendor(CompxAtkVxeBatteryProvider.KeychronVendorId)
              && CompxAtkVxeBatteryProvider.IsSupportedVendor(CompxAtkVxeBatteryProvider.DareuVendorId)
              && CompxAtkVxeBatteryProvider.IsSupportedVendor(CompxAtkVxeBatteryProvider.RapooVendorId),
            "CompxAtkVxeBatteryProvider must recognize Pulsar/Lamzu 8K, Keychron, Dareu, and Rapoo vendor IDs.");

        check(SteelSeriesBatteryProvider.ClassifyProtocol(SteelSeriesBatteryProvider.Nova7Gen2AltPid) == SteelSeriesBatteryProvider.SteelSeriesProtocolKind.Nova7AndArctis7Plus
              && SteelSeriesBatteryProvider.ClassifyProtocol(SteelSeriesBatteryProvider.Nova3PPid) == SteelSeriesBatteryProvider.SteelSeriesProtocolKind.Nova7AndArctis7Plus
              && SteelSeriesBatteryProvider.ClassifyProtocol(SteelSeriesBatteryProvider.Arctis1WirelessPid) == SteelSeriesBatteryProvider.SteelSeriesProtocolKind.Arctis7And9Legacy,
            "SteelSeriesBatteryProvider must classify Nova 7 Gen 2 alt, Nova 3P, and Arctis 1 PIDs.");

        // 7. Regression tests for Argon High P1/P2 fixes:
        // 7a. Corsair Bragi permille low-battery boundary (50 -> 5%, 100 -> 10%)
        byte[] bragi50 = [0x00, 0x02, 0x00, 50, 0x00, 0x00];
        byte[] bragi100 = [0x00, 0x02, 0x00, 100, 0x00, 0x00];
        check(CorsairBatteryProvider.TryParseBragiBatteryLevel(bragi50, out var b50) && b50 == 5
              && CorsairBatteryProvider.TryParseBragiBatteryLevel(bragi100, out var b100) && b100 == 10,
            "Corsair Bragi permille parser must scale 50 -> 5% and 100 -> 10% without jumping to 50%/100%.");

        // 7b. SteelSeries WirelessMouse & Arctis7And9Legacy low-battery direct percentage
        byte[] ssMouse10 = [0xD2, 10, 0x00];
        byte[] ssMouse20 = [0xD2, 20, 0x00];
        byte[] ssLegacy4 = [0x06, 0x18, 4, 0x00];
        check(SteelSeriesBatteryProvider.TryParseProtocolReply(SteelSeriesBatteryProvider.SteelSeriesProtocolKind.WirelessMouse, ssMouse10, "m10", "Aerox", fakeNow, out var m10) && m10?.BatteryLevel == 10
              && SteelSeriesBatteryProvider.TryParseProtocolReply(SteelSeriesBatteryProvider.SteelSeriesProtocolKind.WirelessMouse, ssMouse20, "m20", "Aerox", fakeNow, out var m20) && m20?.BatteryLevel == 20
              && SteelSeriesBatteryProvider.TryParseProtocolReply(SteelSeriesBatteryProvider.SteelSeriesProtocolKind.Arctis7And9Legacy, ssLegacy4, "a4", "Arctis 7", fakeNow, out var a4) && a4?.BatteryLevel == 4,
            "SteelSeries mouse and legacy headset parsers must report 4%, 10%, 20% directly without step-range overlap.");

        // 7c. SteelSeries IsMatchingInterface must reject unknown/wired 0x1038 PIDs
        var unknownSteelSeries = new HidInterfaceDescriptor(@"\\?\hid#vid_1038&pid_9999", 0x1038, 0x9999, 1, 0xFFC0, 1, 64, 64, 0, "SteelSeries Wired Keyboard");
        check(!SteelSeriesBatteryProvider.IsMatchingInterface(unknownSteelSeries),
            "SteelSeriesBatteryProvider must reject unknown/wired 0x1038 PIDs.");

        // 7d. ATK 1K PAW must reject sleeping 0% non-charging reply
        var atk1kZero = (byte[])atk1kReply.Clone();
        atk1kZero[9] = 0x00;
        check(!CompxAtkVxeBatteryProvider.TryParseAtk1kReply(atk1kZero, "atk1k", "ATK 1K PAW", fakeNow, out _),
            "TryParseAtk1kReply must reject sleeping 0% non-charging reply.");

        // 7e. Disk cache future timestamp rejection + same-name promotion ghost eviction
        var tempDir2 = Path.Combine(Path.GetTempPath(), "OpenTaskbar_WirelessCacheGhost_" + Guid.NewGuid().ToString("N"));
        var cachePath2 = Path.Combine(tempDir2, "cache.json");
        try
        {
            var futureEntry = new Dictionary<string, WirelessDeviceReading>
            {
                ["name:future"] = new("bt:f", "Future Device", BluetoothDeviceKind.Mouse, 99, true, false, false, WirelessBatterySource.Bluetooth, fakeNow.AddHours(2))
            };
            Directory.CreateDirectory(tempDir2);
            File.WriteAllText(cachePath2, JsonSerializer.Serialize(futureEntry));
            var futureAgg = new WirelessBatteryAggregator(
                bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>([]),
                providers: [],
                clock: () => fakeNow,
                cacheFilePath: cachePath2);
            check(futureAgg.PollAsync().GetAwaiter().GetResult().Count == 0,
                "Disk cache loader must reject future-dated SampledAt entries.");

            var firstOnly = true;
            var promoAgg = new WirelessBatteryAggregator(
                bluetoothReader: _ => Task.FromResult<IReadOnlyList<WirelessDeviceReading>>(firstOnly
                    ? [new("bt:111", "Same Pad", BluetoothDeviceKind.Gamepad, 80, true, false, false, WirelessBatterySource.Bluetooth, fakeNow)]
                    : [
                        new("bt:111", "Same Pad", BluetoothDeviceKind.Gamepad, 80, true, false, false, WirelessBatterySource.Bluetooth, fakeNow),
                        new("bt:222", "Same Pad", BluetoothDeviceKind.Gamepad, 40, true, false, false, WirelessBatterySource.Bluetooth, fakeNow)
                      ]),
                providers: [],
                clock: () => fakeNow);
            var p1 = promoAgg.PollAsync().GetAwaiter().GetResult();
            firstOnly = false;
            var p2 = promoAgg.PollAsync().GetAwaiter().GetResult();
            check(p1.Count == 1 && p2.Count == 2,
                "Promoting a same-name device from name: key to src:id: key must not emit a duplicate 3rd stale ghost.");
        }
        finally
        {
            try { if (Directory.Exists(tempDir2)) Directory.Delete(tempDir2, recursive: true); } catch { }
        }
    }

    private static int RunUiChecksOnSta(bool includeLegacyBluetoothSuite)
    {
        Exception? failure = null;
        var checks = 0;
        var thread = new Thread(() =>
        {
            try
            {
                checks = VerifyWidgetUi(
                    (cond, msg) =>
                    {
                        if (!cond) throw new InvalidOperationException(msg);
                    },
                    includeLegacyBluetoothSuite);
            }
            catch (Exception ex)
            {
                failure = ex;
            }
            finally
            {
                System.Windows.Threading.Dispatcher.CurrentDispatcher.InvokeShutdown();
            }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        if (failure is not null) ExceptionDispatchInfo.Capture(failure).Throw();
        return checks;
    }

    private static void EnsureWidgetResources()
    {
        var app = Application.Current ?? new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
        if (app.TryFindResource("TextPrimary") is not null)
            return;

        foreach (var resource in new[]
                 {
                     "Tokens/Typography", "Tokens/Spacing", "Tokens/Radii", "Tokens/Layout", "Tokens/Motion", "Tokens/Effects",
                     "Palettes/Dark",
                     "Icons/Navigation", "Icons/Actions", "Icons/Status", "Icons/Media", "Icons/Brand", "Icons/GeometryTemplate",
                     "Controls/Tooltip", "Controls/ScrollBar",
                     "DockControls", "ContextMenus", "Widgets", "WidgetControls", "SettingsControls"
                 })
        {
            app.Resources.MergedDictionaries.Add(new ResourceDictionary
            {
                Source = new Uri($"/OpenTaskbar;component/Design/{resource}.xaml", UriKind.Relative)
            });
        }
    }

    private static int VerifyWidgetUi(Action<bool, string> check, bool includeLegacyBluetoothSuite)
    {
        EnsureWidgetResources();
        var startChecks = 0;
        void LocalCheck(bool condition, string message)
        {
            check(condition, message);
            startChecks++;
        }

        var sampledAt = DateTimeOffset.UtcNow.AddMinutes(-4);
        IReadOnlyList<BluetoothDeviceBatteryInfo> currentDevices =
        [
            new("ATK A9 Ultra", "compx:373B:1119", true, 100, BluetoothDeviceKind.Mouse,
                IsCharging: true, IsStale: false, SourceLabel: "ATK/VXE", SampledAt: DateTimeOffset.UtcNow),
            new("Arctis Nova Pro Wireless", "steelseries:1038:12E0", true, 75, BluetoothDeviceKind.Headphones,
                IsCharging: false, IsStale: true, SourceLabel: "SteelSeries", SampledAt: sampledAt, SecondaryBatteryLevel: 100),
            new("Xbox Wireless Controller (1)", "xinput:slot:0", true, 20, BluetoothDeviceKind.Gamepad,
                IsCharging: false, IsStale: false, SourceLabel: "XInput", LevelOverrideText: "Low")
        ];

        var notifications = new List<string>();
        string? copiedDiagnostics = null;

        var widget = new BluetoothBatteryWidget
        {
            BatteryReader = () => Task.FromResult(currentDevices),
            LowBatteryNotifier = msg => notifications.Add(msg),
            DiagnosticsProvider = () => "VID:373B PID:1119 UsagePage:0xFF02 Usage:0x0002 In:17 Out:17 Feat:0",
            ClipboardSetter = text => copiedDiagnostics = text
        };

        widget.Activate();
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();

        var button = (Button)widget.Content!;
        var tiles = (StackPanel)button.Content!;
        LocalCheck(tiles.Children.Count == 3, "All 3 connected wireless devices must render as dock tiles.");

        // Diff-based UI rebuild skip verification:
        var rebuildsAfterFirst = widget.TileRebuildCount;
        var firstTileRef = tiles.Children[0];
        LocalCheck(rebuildsAfterFirst == 1, "Initial render must perform exactly 1 tile rebuild.");

        // Steady-state refresh with identical device state must skip WPF element reconstruction!
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();
        LocalCheck(widget.TileRebuildCount == 1 && ReferenceEquals(firstTileRef, tiles.Children[0]),
            "Steady-state refresh with unchanged device readings must skip _deviceTiles.Children.Clear() and reuse existing WPF elements.");

        // Tile 0: Charging mouse must preserve stack.Children[1] as TextBlock ("100%") and append charging bolt at index 2.
        var tile0 = (Border)tiles.Children[0];
        var stack0 = (StackPanel)tile0.Child!;
        LocalCheck(stack0.Children[1] is TextBlock { Text: "100%" } && stack0.Children.Count == 3,
            "Charging device tile must keep percentage TextBlock at stack.Children[1] and append charging bolt at index 2.");
        LocalCheck(Math.Abs(tile0.Opacity - 1.0) < 0.01, "Live non-stale tile must render at full opacity.");

        // Tile 1: Stale headset must render with dimmed opacity.
        var tile1 = (Border)tiles.Children[1];
        var stack1 = (StackPanel)tile1.Child!;
        LocalCheck(stack1.Children[1] is TextBlock { Text: "75%" } && tile1.Opacity < 0.8,
            "Stale device tile must render with dimmed opacity while preserving percentage TextBlock at index 1.");

        // Tile 2: XInput coarse level override must render "Low" at index 1.
        var tile2 = (Border)tiles.Children[2];
        var stack2 = (StackPanel)tile2.Child!;
        LocalCheck(stack2.Children[1] is TextBlock { Text: "Low" },
            "Coarse LevelOverrideText must be displayed in stack.Children[1].");

        // Test device alias rename & ignore/restore
        widget.SetDeviceAlias("compx:373B:1119", "My Pro Mouse");
        LocalCheck(widget.TileRebuildCount == 2
                   && button.ToolTip?.ToString()?.Contains("My Pro Mouse: 100%") == true,
            "Setting a custom device alias must trigger a diff rebuild and update dock tooltip.");

        widget.HideDevice("xinput:slot:0");
        LocalCheck(tiles.Children.Count == 2,
            "Hiding a wireless device must immediately remove its tile from the dock.");

        widget.RestoreIgnoredDevices();
        LocalCheck(tiles.Children.Count == 3,
            "Restoring hidden wireless devices must bring back all 3 connected tiles.");

        // Test Low-Battery Notification spam-guard & charging suppression
        widget.ApplySettings(new DockSettings(WirelessBatteryLowThreshold: 20));
        // Xbox controller is at 20% and non-charging -> should fire exactly 1 notification
        LocalCheck(notifications.Count == 1 && notifications[0].Contains("Xbox Wireless Controller (1)"),
            "Enabling 20% low-battery threshold must notify once for the non-charging 20% controller.");

        // Second refresh at same 20% level must NOT spam a duplicate notification
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();
        LocalCheck(notifications.Count == 1,
            "Low-battery notification spam guard must suppress duplicate alerts at the same threshold.");

        // If a device is at 10% but charging, it must NOT trigger a low-battery toast
        currentDevices =
        [
            new("ATK A9 Ultra", "compx:373B:1119", true, 10, BluetoothDeviceKind.Mouse, IsCharging: true)
        ];
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();
        LocalCheck(notifications.Count == 1,
            "Charging devices must never trigger low-battery toast notifications.");

        // Restore 3 devices for flyout check
        currentDevices =
        [
            new("ATK A9 Ultra", "compx:373B:1119", true, 100, BluetoothDeviceKind.Mouse,
                IsCharging: true, IsStale: false, SourceLabel: "ATK/VXE", SampledAt: DateTimeOffset.UtcNow),
            new("Arctis Nova Pro Wireless", "steelseries:1038:12E0", true, 75, BluetoothDeviceKind.Headphones,
                IsCharging: false, IsStale: true, SourceLabel: "SteelSeries", SampledAt: sampledAt, SecondaryBatteryLevel: 100),
            new("Xbox Wireless Controller (1)", "xinput:slot:0", true, 20, BluetoothDeviceKind.Gamepad,
                IsCharging: false, IsStale: false, SourceLabel: "XInput", LevelOverrideText: "Low")
        ];
        widget.SetDeviceAlias("compx:373B:1119", null);
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();

        // Copy diagnostics action check
        var diagResult = widget.CopyDiagnosticsToClipboard();
        LocalCheck(copiedDiagnostics == "VID:373B PID:1119 UsagePage:0xFF02 Usage:0x0002 In:17 Out:17 Feat:0"
                   && diagResult == copiedDiagnostics,
            "CopyDiagnosticsToClipboard must copy the PII-free VID/PID/UsagePage summary.");

        // Flyout verification & live in-place flyout refresh on settings/device changes
        var flyout = widget.BuildHoverFlyoutContent();
        LocalCheck(flyout is StackPanel, "Hover flyout root must remain a StackPanel.");
        var allTexts = CollectTextBlocks((DependencyObject)flyout!).Select(t => t.Text).ToList();
        LocalCheck(allTexts.Contains("ATK A9 Ultra") && allTexts.Contains("Arctis Nova Pro Wireless") && allTexts.Contains("Xbox Wireless Controller (1)"),
            "Hover flyout must display all wireless devices cleanly without protocol badge clutter.");
        LocalCheck(allTexts.Any(t => t.Contains("100", StringComparison.Ordinal)),
            "Hover flyout must display spare dock battery badge and percentages.");
        LocalCheck(allTexts.Contains(UiText.Get("WirelessBatteryCopyDiagnostics")),
            "Hover flyout must include the Copy Diagnostic IDs action.");

        // Verify live open flyout refreshes in-place when Poll, Threshold, Alias, Hide, and Restore are triggered,
        // and verify UpdateSettings applies settings exactly once per change (no triple ApplySettings).
        DockSettings? savedByHost = null;
        widget.UpdateSettings = updated =>
        {
            savedByHost = updated;
            widget.ApplySettings(updated);
        };
        var applyBefore = widget.SettingsApplyCount;
        widget.CyclePollInterval();
        LocalCheck(widget.SettingsApplyCount == applyBefore + 1 && savedByHost?.WirelessBatteryPollSeconds == 60,
            "CyclePollInterval via UpdateSettings must invoke OnSettingsChanged exactly once and update poll seconds to 60.");
        var textsAfterPoll = CollectTextBlocks((DependencyObject)flyout!).Select(t => t.Text).ToList();
        LocalCheck(textsAfterPoll.Contains(UiText.Format("WirelessBatteryPollInterval", 60)),
            "Open hover flyout must immediately update the Poll Interval button label in place.");

        widget.SetDeviceAlias("compx:373B:1119", "Flyout Renamed Mouse");
        var textsAfterRename = CollectTextBlocks((DependencyObject)flyout!).Select(t => t.Text).ToList();
        LocalCheck(textsAfterRename.Contains("Flyout Renamed Mouse"),
            "Open hover flyout must immediately reflect a renamed device alias in place.");

        widget.HideDevice("xinput:slot:0");
        var textsAfterHide = CollectTextBlocks((DependencyObject)flyout!).Select(t => t.Text).ToList();
        LocalCheck(!textsAfterHide.Contains("Xbox Wireless Controller (1)")
                   && textsAfterHide.Contains(UiText.Format("WirelessBatteryRestoreHidden", 1)),
            "Open hover flyout must immediately remove a hidden device row and display the Restore Hidden button in place.");

        widget.RestoreIgnoredDevices();
        widget.SetDeviceAlias("compx:373B:1119", null);
        var textsAfterRestore = CollectTextBlocks((DependencyObject)flyout!).Select(t => t.Text).ToList();
        LocalCheck(textsAfterRestore.Contains("Xbox Wireless Controller (1)") && textsAfterRestore.Contains("ATK A9 Ultra"),
            "Open hover flyout must immediately restore unhidden device rows in place.");

        // Regression test: XInput coarse level text must re-localize immediately on language switch without waiting for a new poll.
        var originalLanguage = UiText.Current.Language;
        try
        {
            UiText.Current.SetLanguage(AppLanguage.English);
            var englishLow = UiText.Get("WirelessBatteryLevelLow");
            var xinputStack = (StackPanel)((Border)tiles.Children[2]).Child!;
            LocalCheck(((TextBlock)xinputStack.Children[1]).Text == englishLow,
                "XInput tile must display English 'Low' after switching language to English.");

            UiText.Current.SetLanguage(AppLanguage.Turkish);
            var turkishLow = UiText.Get("WirelessBatteryLevelLow");
            xinputStack = (StackPanel)((Border)tiles.Children[2]).Child!;
            LocalCheck(turkishLow != englishLow && ((TextBlock)xinputStack.Children[1]).Text == turkishLow,
                "XInput tile must immediately re-localize to Turkish 'Düşük' on language change without re-polling.");
        }
        finally
        {
            UiText.Current.SetLanguage(originalLanguage);
        }

        widget.Deactivate();
        if (includeLegacyBluetoothSuite && Application.Current is { } app)
        {
            startChecks += BluetoothBatteryChecks.Run(app);
        }
        return startChecks;
    }

    private static IEnumerable<TextBlock> CollectTextBlocks(DependencyObject root)
    {
        if (root is TextBlock tb) yield return tb;
        foreach (var child in LogicalTreeHelper.GetChildren(root).OfType<DependencyObject>())
        {
            foreach (var nested in CollectTextBlocks(child))
                yield return nested;
        }
    }

    private sealed class DelegateBatteryProvider(
        WirelessBatterySource source,
        Func<CancellationToken, Task<IReadOnlyList<WirelessDeviceReading>>> probe) : IWirelessBatteryProvider
    {
        public WirelessBatterySource Source => source;
        public Task<IReadOnlyList<WirelessDeviceReading>> ProbeAsync(
            IReadOnlyList<HidInterfaceDescriptor> hidInterfaces,
            CancellationToken cancellationToken) => probe(cancellationToken);
    }
}
