using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace OpenTaskbar.Services;

internal sealed record HidBatteryValueCap(
    bool IsFeatureReport,
    byte ReportId,
    ushort UsagePage,
    ushort Usage,
    ushort LinkCollection,
    int LogicalMin,
    int LogicalMax);

internal sealed record HidInterfaceDescriptor(
    string DevicePath,
    ushort VendorId,
    ushort ProductId,
    ushort VersionNumber,
    ushort UsagePage,
    ushort Usage,
    ushort InputReportLength,
    ushort OutputReportLength,
    ushort FeatureReportLength,
    string ProductName,
    IReadOnlyList<HidBatteryValueCap>? BatteryValueCaps = null);

/// <summary>
/// Clean-room Win32 SetupAPI + HidD/HidP + Overlapped I/O transport for reading wireless peripheral
/// battery reports without blocking threads or acquiring exclusive device locks.
/// </summary>
internal static class Win32HidTransport
{
    private const uint DIGCF_PRESENT = 0x00000002;
    private const uint DIGCF_DEVICEINTERFACE = 0x00000010;

    private const uint GENERIC_READ = 0x80000000;
    private const uint GENERIC_WRITE = 0x40000000;
    private const uint FILE_SHARE_READ = 0x00000001;
    private const uint FILE_SHARE_WRITE = 0x00000002;
    private const uint OPEN_EXISTING = 3;
    private const uint FILE_FLAG_OVERLAPPED = 0x40000000;

    internal const int ERROR_IO_PENDING = 997;
    internal const int ERROR_NOT_FOUND = 1168;
    internal const int ERROR_TIMEOUT = 1460;
    internal const int ERROR_INVALID_PARAMETER = 87;
    internal const int ERROR_ACCESS_DENIED = 5;
    private const uint WAIT_OBJECT_0 = 0x00000000;
    internal const int HIDP_STATUS_SUCCESS = 0x00110000;

    internal const int HidP_Input = 0;
    internal const int HidP_Output = 1;
    internal const int HidP_Feature = 2;

    public static IReadOnlyList<HidInterfaceDescriptor> EnumerateInterfaces(Func<ushort, ushort, bool>? vidPidFilter = null)
    {
        var results = new List<HidInterfaceDescriptor>();
        HidD_GetHidGuid(out var hidGuid);
        var infoSet = SetupDiGetClassDevsW(ref hidGuid, nint.Zero, nint.Zero, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if (infoSet == nint.Zero || infoSet == -1) return results;

        try
        {
            uint index = 0;
            var interfaceDataSize = (uint)Marshal.SizeOf<SP_DEVICE_INTERFACE_DATA>();
            while (true)
            {
                var interfaceData = new SP_DEVICE_INTERFACE_DATA { cbSize = interfaceDataSize };
                if (!SetupDiEnumDeviceInterfaces(infoSet, nint.Zero, ref hidGuid, index++, ref interfaceData))
                    break;

                SetupDiGetDeviceInterfaceDetailW(infoSet, ref interfaceData, nint.Zero, 0, out var requiredSize, nint.Zero);
                if (requiredSize is 0 or > 4096) continue;

                var detailBuffer = Marshal.AllocHGlobal((int)requiredSize);
                try
                {
                    Marshal.WriteInt32(detailBuffer, nint.Size == 8 ? 8 : 6);
                    if (!SetupDiGetDeviceInterfaceDetailW(infoSet, ref interfaceData, detailBuffer, requiredSize, out _, nint.Zero))
                        continue;

                    var devicePath = Marshal.PtrToStringUni(nint.Add(detailBuffer, 4));
                    if (string.IsNullOrWhiteSpace(devicePath)) continue;

                    using var metaHandle = CreateFileW(
                        devicePath,
                        dwDesiredAccess: 0,
                        dwShareMode: FILE_SHARE_READ | FILE_SHARE_WRITE,
                        lpSecurityAttributes: nint.Zero,
                        dwCreationDisposition: OPEN_EXISTING,
                        dwFlagsAndAttributes: 0,
                        hTemplateFile: nint.Zero);

                    if (metaHandle.IsInvalid) continue;

                    var attributes = new HIDD_ATTRIBUTES { Size = (uint)Marshal.SizeOf<HIDD_ATTRIBUTES>() };
                    if (!HidD_GetAttributes(metaHandle, ref attributes)) continue;

                    if (vidPidFilter is not null && !vidPidFilter(attributes.VendorID, attributes.ProductID))
                        continue;

                    if (!HidD_GetPreparsedData(metaHandle, out var preparsedData) || preparsedData == nint.Zero)
                        continue;

                    HIDP_CAPS caps;
                    IReadOnlyList<HidBatteryValueCap>? batteryCaps = null;
                    try
                    {
                        if (HidP_GetCaps(preparsedData, out caps) != HIDP_STATUS_SUCCESS)
                            continue;

                        batteryCaps = ExtractStandardBatteryValueCaps(preparsedData, caps);
                    }
                    finally
                    {
                        HidD_FreePreparsedData(preparsedData);
                    }

                    var productName = ReadProductString(metaHandle);
                    results.Add(new HidInterfaceDescriptor(
                        DevicePath: devicePath,
                        VendorId: attributes.VendorID,
                        ProductId: attributes.ProductID,
                        VersionNumber: attributes.VersionNumber,
                        UsagePage: caps.UsagePage,
                        Usage: caps.Usage,
                        InputReportLength: caps.InputReportByteLength,
                        OutputReportLength: caps.OutputReportByteLength,
                        FeatureReportLength: caps.FeatureReportByteLength,
                        ProductName: productName,
                        BatteryValueCaps: batteryCaps));
                }
                finally
                {
                    Marshal.FreeHGlobal(detailBuffer);
                }
            }
        }
        finally
        {
            SetupDiDestroyDeviceInfoList(infoSet);
        }

        return results;
    }

    private static IReadOnlyList<HidBatteryValueCap>? ExtractStandardBatteryValueCaps(nint preparsedData, HIDP_CAPS caps)
    {
        List<HidBatteryValueCap>? matches = null;
        ExtractFromReportType(preparsedData, HidP_Feature, caps.NumberFeatureValueCaps, isFeature: true, ref matches);
        ExtractFromReportType(preparsedData, HidP_Input, caps.NumberInputValueCaps, isFeature: false, ref matches);
        return matches;
    }

    private static void ExtractFromReportType(
        nint preparsedData,
        int reportType,
        ushort count,
        bool isFeature,
        ref List<HidBatteryValueCap>? matches)
    {
        if (count is 0 or > 64) return;
        var length = count;
        var valueCaps = new HIDP_VALUE_CAPS[length];
        if (HidP_GetValueCaps(reportType, valueCaps, ref length, preparsedData) != HIDP_STATUS_SUCCESS)
            return;

        for (var i = 0; i < length && i < valueCaps.Length; i++)
        {
            var vc = valueCaps[i];
            if (!TryMatchBatteryUsage(vc, out var usage))
                continue;

            matches ??= new List<HidBatteryValueCap>(2);
            matches.Add(new HidBatteryValueCap(
                IsFeatureReport: isFeature,
                ReportId: vc.ReportID,
                UsagePage: vc.UsagePage,
                Usage: usage,
                LinkCollection: vc.LinkCollection,
                LogicalMin: vc.LogicalMin,
                LogicalMax: vc.LogicalMax));
        }
    }

    private static bool TryMatchBatteryUsage(HIDP_VALUE_CAPS vc, out ushort matchedUsage)
    {
        matchedUsage = 0;
        ushort[] targetUsages = vc.UsagePage switch
        {
            0x0006 => [0x0020], // Generic Device Controls: Battery Strength
            0x0084 => [0x0065, 0x00D0], // Power Device: Capacity / AC Present
            0x0085 => [0x0064, 0x0065, 0x0066, 0x0044, 0x0045], // Battery System: Rel/Abs StateOfCharge, RemainingCapacity, Charging, Discharging
            _ => []
        };

        if (targetUsages.Length == 0) return false;

        foreach (var target in targetUsages)
        {
            if (vc.IsRange == 0)
            {
                if (vc.UsageOrUsageMin == target)
                {
                    matchedUsage = target;
                    return true;
                }
            }
            else if (target >= vc.UsageOrUsageMin && target <= vc.UsageMax)
            {
                matchedUsage = target;
                return true;
            }
        }

        return false;
    }

    public static bool TryOpenSession(
        string devicePath,
        bool featureOnly,
        out HidDeviceSession? session,
        out int win32Error)
    {
        session = null;
        var access = featureOnly ? 0u : (GENERIC_READ | GENERIC_WRITE);
        var flags = featureOnly ? 0u : FILE_FLAG_OVERLAPPED;
        var handle = CreateFileW(
            devicePath,
            dwDesiredAccess: access,
            dwShareMode: FILE_SHARE_READ | FILE_SHARE_WRITE,
            lpSecurityAttributes: nint.Zero,
            dwCreationDisposition: OPEN_EXISTING,
            dwFlagsAndAttributes: flags,
            hTemplateFile: nint.Zero);

        if (handle.IsInvalid && featureOnly)
        {
            handle.Dispose();
            handle = CreateFileW(
                devicePath,
                dwDesiredAccess: GENERIC_READ | GENERIC_WRITE,
                dwShareMode: FILE_SHARE_READ | FILE_SHARE_WRITE,
                lpSecurityAttributes: nint.Zero,
                dwCreationDisposition: OPEN_EXISTING,
                dwFlagsAndAttributes: 0,
                hTemplateFile: nint.Zero);
        }

        if (handle.IsInvalid)
        {
            win32Error = Marshal.GetLastWin32Error();
            handle.Dispose();
            return false;
        }

        if (!featureOnly && IsSystemMouseOrKeyboardCollection(handle))
        {
            handle.Dispose();
            win32Error = ERROR_ACCESS_DENIED;
            return false;
        }

        win32Error = 0;
        session = new HidDeviceSession(handle);
        return true;
    }

    private static bool IsSystemMouseOrKeyboardCollection(SafeFileHandle handle)
    {
        if (!HidD_GetPreparsedData(handle, out var preparsedData) || preparsedData == nint.Zero)
            return false;
        try
        {
            if (HidP_GetCaps(preparsedData, out var caps) != HIDP_STATUS_SUCCESS)
                return false;
            return caps.UsagePage == 0x0001 && caps.Usage is 0x0002 or 0x0006;
        }
        finally
        {
            HidD_FreePreparsedData(preparsedData);
        }
    }

    private static string ReadProductString(SafeFileHandle handle)
    {
        var buffer = new byte[256];
        if (!HidD_GetProductString(handle, buffer, (uint)buffer.Length))
            return string.Empty;

        var text = Encoding.Unicode.GetString(buffer);
        var nullIndex = text.IndexOf('\0');
        return (nullIndex >= 0 ? text[..nullIndex] : text).Trim();
    }

    internal sealed class HidDeviceSession : IDisposable
    {
        private readonly SafeFileHandle _handle;
        private bool _disposed;

        public bool IsDisposed => _disposed;

        internal HidDeviceSession(SafeFileHandle handle)
        {
            _handle = handle;
        }

        public bool TryWriteReport(ReadOnlySpan<byte> report, int timeoutMs, out int win32Error)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            if (report.IsEmpty)
            {
                win32Error = ERROR_INVALID_PARAMETER;
                return false;
            }

            var length = report.Length;
            var bufferPtr = Marshal.AllocHGlobal(length);
            var managed = report.ToArray();
            Marshal.Copy(managed, 0, bufferPtr, length);

            var ok = TryExecuteOverlappedIo(
                isWrite: true,
                bufferPtr: bufferPtr,
                bufferLength: (uint)length,
                timeoutMs: timeoutMs,
                out var transferred,
                out win32Error);
            return ok && transferred > 0;
        }

        public bool TryReadReport(Span<byte> destination, int timeoutMs, out int bytesRead, out int win32Error)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            bytesRead = 0;
            if (destination.IsEmpty)
            {
                win32Error = ERROR_INVALID_PARAMETER;
                return false;
            }

            var length = destination.Length;
            var bufferPtr = Marshal.AllocHGlobal(length);
            var ok = TryExecuteOverlappedIo(
                isWrite: false,
                bufferPtr: bufferPtr,
                bufferLength: (uint)length,
                timeoutMs: timeoutMs,
                out var transferred,
                out win32Error,
                destination);

            if (!ok || transferred == 0)
                return false;

            bytesRead = (int)Math.Min(transferred, (uint)length);
            return true;
        }

        private bool TryExecuteOverlappedIo(
            bool isWrite,
            nint bufferPtr,
            uint bufferLength,
            int timeoutMs,
            out uint transferred,
            out int win32Error,
            Span<byte> readDestination = default)
        {
            transferred = 0;
            var clampedTimeout = Math.Clamp(timeoutMs, 50, 300);
            var hEvent = CreateEventW(nint.Zero, bManualReset: true, bInitialState: false, lpName: null);
            if (hEvent == nint.Zero)
            {
                win32Error = Marshal.GetLastWin32Error();
                Marshal.FreeHGlobal(bufferPtr);
                return false;
            }

            var overlappedSize = Marshal.SizeOf<OVERLAPPED>();
            var overlappedPtr = Marshal.AllocHGlobal(overlappedSize);
            for (var i = 0; i < overlappedSize; i++)
                Marshal.WriteByte(overlappedPtr, i, 0);
            Marshal.StructureToPtr(new OVERLAPPED { hEvent = hEvent }, overlappedPtr, fDeleteOld: false);

            var ownsResources = true;
            try
            {
                var ok = isWrite
                    ? WriteFile(_handle, bufferPtr, bufferLength, nint.Zero, overlappedPtr)
                    : ReadFile(_handle, bufferPtr, bufferLength, nint.Zero, overlappedPtr);
                var err = Marshal.GetLastWin32Error();

                if (!ok)
                {
                    if (err == ERROR_IO_PENDING)
                    {
                        var wait = WaitForSingleObject(hEvent, (uint)clampedTimeout);
                        if (wait == WAIT_OBJECT_0)
                        {
                            ok = GetOverlappedResult(_handle, overlappedPtr, out transferred, bWait: false);
                            err = ok ? 0 : Marshal.GetLastWin32Error();
                        }
                        else
                        {
                            var cancelled = CancelIoEx(_handle, overlappedPtr);
                            var cancelErr = cancelled ? 0 : Marshal.GetLastWin32Error();
                            // Never wait without a bound here: a stuck HID driver may never complete the
                            // cancelled IRP, which would pin a thread-pool worker forever.
                            var drainWait = WaitForSingleObject(hEvent, 250);
                            if (drainWait == WAIT_OBJECT_0 || (!cancelled && cancelErr == ERROR_NOT_FOUND))
                            {
                                GetOverlappedResult(_handle, overlappedPtr, out _, bWait: false);
                            }
                            else
                            {
                                // The kernel IRP may still hold overlappedPtr/bufferPtr; defer freeing until hEvent signals,
                                // and immediately close the wedged session handle so retry loops cannot issue more I/O on it.
                                ownsResources = false;
                                DeferCleanupUntilSignaled(hEvent, overlappedPtr, bufferPtr);
                                Dispose();
                            }

                            win32Error = ERROR_TIMEOUT;
                            return false;
                        }
                    }
                    else
                    {
                        win32Error = err;
                        return false;
                    }
                }
                else
                {
                    ok = GetOverlappedResult(_handle, overlappedPtr, out transferred, bWait: false);
                    err = ok ? 0 : Marshal.GetLastWin32Error();
                }

                win32Error = err;
                if (!isWrite && ok && transferred > 0)
                {
                    var count = (int)Math.Min(transferred, (uint)readDestination.Length);
                    var managed = new byte[count];
                    Marshal.Copy(bufferPtr, managed, 0, count);
                    managed.CopyTo(readDestination);
                }

                return ok;
            }
            finally
            {
                if (ownsResources)
                {
                    CloseHandle(hEvent);
                    Marshal.FreeHGlobal(overlappedPtr);
                    Marshal.FreeHGlobal(bufferPtr);
                }
            }
        }

        private sealed class DeferredCleanupState(DeferredRawEventWaitHandle handle, nint overlappedPtr, nint bufferPtr)
        {
            public readonly DeferredRawEventWaitHandle Handle = handle;
            public readonly nint OverlappedPtr = overlappedPtr;
            public readonly nint BufferPtr = bufferPtr;
            private RegisteredWaitHandle? _registered;
            private int _callbackCompleted;
            private int _unregistered;

            public void AttachRegistration(RegisteredWaitHandle registered)
            {
                Volatile.Write(ref _registered, registered);
                // If the callback already ran before AttachRegistration completed, unregister now.
                if (Volatile.Read(ref _callbackCompleted) != 0)
                    TryUnregister();
            }

            public void CompleteAndFree()
            {
                Volatile.Write(ref _callbackCompleted, 1);
                TryUnregister();
                try { Handle.Dispose(); } catch { }
                CloseHandle(Handle.DangerousRawHandle);
                Marshal.FreeHGlobal(OverlappedPtr);
                Marshal.FreeHGlobal(BufferPtr);
            }

            private void TryUnregister()
            {
                if (Volatile.Read(ref _registered) is { } reg && Interlocked.Exchange(ref _unregistered, 1) == 0)
                {
                    try { reg.Unregister(null); } catch { }
                }
            }
        }

        private static void DeferCleanupUntilSignaled(nint hEvent, nint overlappedPtr, nint bufferPtr)
        {
            try
            {
                var waitHandle = new DeferredRawEventWaitHandle(hEvent);
                var state = new DeferredCleanupState(waitHandle, overlappedPtr, bufferPtr);
                var registered = ThreadPool.RegisterWaitForSingleObject(
                    waitHandle,
                    static (rawState, _) => ((DeferredCleanupState)rawState!).CompleteAndFree(),
                    state,
                    Timeout.Infinite,
                    executeOnlyOnce: true);
                state.AttachRegistration(registered);
            }
            catch
            {
                // Intentionally leak unmanaged pointers rather than risking a kernel use-after-free on registration failure.
            }
        }

        private sealed class DeferredRawEventWaitHandle : WaitHandle
        {
            public nint DangerousRawHandle { get; }

            public DeferredRawEventWaitHandle(nint rawHandle)
            {
                DangerousRawHandle = rawHandle;
                SafeWaitHandle = new SafeWaitHandle(rawHandle, ownsHandle: false);
            }
        }

        public bool TrySetOutputReport(ReadOnlySpan<byte> report, out int win32Error)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            var buffer = report.ToArray();
            var ok = HidD_SetOutputReport(_handle, buffer, (uint)buffer.Length);
            win32Error = ok ? 0 : Marshal.GetLastWin32Error();
            return ok;
        }

        public bool TryGetInputReport(Span<byte> report, out int win32Error)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            var buffer = report.ToArray();
            var ok = HidD_GetInputReport(_handle, buffer, (uint)buffer.Length);
            win32Error = ok ? 0 : Marshal.GetLastWin32Error();
            if (ok) buffer.CopyTo(report);
            return ok;
        }

        public bool TrySetFeature(ReadOnlySpan<byte> report, out int win32Error)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            var buffer = report.ToArray();
            var ok = HidD_SetFeature(_handle, buffer, (uint)buffer.Length);
            win32Error = ok ? 0 : Marshal.GetLastWin32Error();
            return ok;
        }

        public bool TryGetFeature(Span<byte> report, out int win32Error)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            var buffer = report.ToArray();
            var ok = HidD_GetFeature(_handle, buffer, (uint)buffer.Length);
            win32Error = ok ? 0 : Marshal.GetLastWin32Error();
            if (ok) buffer.CopyTo(report);
            return ok;
        }

        public bool TryExtractUsageValue(
            bool isFeatureReport,
            ushort usagePage,
            ushort linkCollection,
            ushort usage,
            byte[] reportBuffer,
            out uint usageValue)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            usageValue = 0;
            if (!HidD_GetPreparsedData(_handle, out var preparsedData) || preparsedData == nint.Zero)
                return false;

            try
            {
                var reportType = isFeatureReport ? HidP_Feature : HidP_Input;
                return HidP_GetUsageValue(
                    reportType,
                    usagePage,
                    linkCollection,
                    usage,
                    out usageValue,
                    preparsedData,
                    reportBuffer,
                    (uint)reportBuffer.Length) == HIDP_STATUS_SUCCESS;
            }
            finally
            {
                HidD_FreePreparsedData(preparsedData);
            }
        }

        public bool TryTransactInterrupt(
            ReadOnlySpan<byte> request,
            Span<byte> response,
            int timeoutMs,
            Func<ReadOnlySpan<byte>, bool>? validator,
            out int bytesRead,
            out int win32Error)
        {
            bytesRead = 0;
            if (_disposed)
            {
                win32Error = ERROR_TIMEOUT;
                return false;
            }
            var clampedTimeout = Math.Clamp(timeoutMs, 100, 300);
            if (!TryWriteReport(request, clampedTimeout, out win32Error))
            {
                if (_disposed || !TrySetOutputReport(request, out win32Error))
                    return false;
            }

            var startTick = Stopwatch.GetTimestamp();
            var maxAttempts = 4;
            for (var attempt = 0; attempt < maxAttempts; attempt++)
            {
                if (_disposed)
                {
                    win32Error = ERROR_TIMEOUT;
                    return false;
                }
                var elapsedMs = (int)Stopwatch.GetElapsedTime(startTick).TotalMilliseconds;
                var remainingMs = clampedTimeout - elapsedMs;
                if (remainingMs < 40) break;

                if (!TryReadReport(response, remainingMs, out bytesRead, out win32Error))
                    return false;

                if (validator is null || validator(response[..bytesRead]))
                    return true;
            }

            win32Error = ERROR_TIMEOUT;
            return false;
        }

        public void Dispose()
        {
            if (_disposed) return;
            _disposed = true;
            _handle.Dispose();
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct SP_DEVICE_INTERFACE_DATA
    {
        public uint cbSize;
        public Guid InterfaceClassGuid;
        public uint Flags;
        public nint Reserved;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct HIDD_ATTRIBUTES
    {
        public uint Size;
        public ushort VendorID;
        public ushort ProductID;
        public ushort VersionNumber;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct HIDP_CAPS
    {
        public ushort Usage;
        public ushort UsagePage;
        public ushort InputReportByteLength;
        public ushort OutputReportByteLength;
        public ushort FeatureReportByteLength;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 17)]
        public ushort[] Reserved;
        public ushort NumberLinkCollectionNodes;
        public ushort NumberInputButtonCaps;
        public ushort NumberInputValueCaps;
        public ushort NumberInputDataIndices;
        public ushort NumberOutputButtonCaps;
        public ushort NumberOutputValueCaps;
        public ushort NumberOutputDataIndices;
        public ushort NumberFeatureButtonCaps;
        public ushort NumberFeatureValueCaps;
        public ushort NumberFeatureDataIndices;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct HIDP_VALUE_CAPS
    {
        public ushort UsagePage;
        public byte ReportID;
        public byte IsAlias;
        public ushort BitField;
        public ushort LinkCollection;
        public ushort LinkUsage;
        public ushort LinkUsagePage;
        public byte IsRange;
        public byte IsStringRange;
        public byte IsDesignatorRange;
        public byte IsAbsolute;
        public byte HasNull;
        public byte Reserved;
        public ushort BitSize;
        public ushort ReportCount;
        public ushort Reserved2_0;
        public ushort Reserved2_1;
        public ushort Reserved2_2;
        public ushort Reserved2_3;
        public ushort Reserved2_4;
        public uint UnitsExp;
        public uint Units;
        public int LogicalMin;
        public int LogicalMax;
        public int PhysicalMin;
        public int PhysicalMax;
        public ushort UsageOrUsageMin;
        public ushort UsageMax;
        public ushort StringMin;
        public ushort StringMax;
        public ushort DesignatorMin;
        public ushort DesignatorMax;
        public ushort DataIndexMin;
        public ushort DataIndexMax;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct OVERLAPPED
    {
        public nint Internal;
        public nint InternalHigh;
        public uint Offset;
        public uint OffsetHigh;
        public nint hEvent;
    }

    [DllImport("hid.dll", SetLastError = true)]
    private static extern void HidD_GetHidGuid(out Guid hidGuid);

    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern nint SetupDiGetClassDevsW(
        ref Guid classGuid,
        nint enumerator,
        nint hwndParent,
        uint flags);

    [DllImport("setupapi.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetupDiEnumDeviceInterfaces(
        nint deviceInfoSet,
        nint deviceInfoData,
        ref Guid interfaceClassGuid,
        uint memberIndex,
        ref SP_DEVICE_INTERFACE_DATA deviceInterfaceData);

    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetupDiGetDeviceInterfaceDetailW(
        nint deviceInfoSet,
        ref SP_DEVICE_INTERFACE_DATA deviceInterfaceData,
        nint deviceInterfaceDetailData,
        uint deviceInterfaceDetailDataSize,
        out uint requiredSize,
        nint deviceInfoData);

    [DllImport("setupapi.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetupDiDestroyDeviceInfoList(nint deviceInfoSet);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(
        string lpFileName,
        uint dwDesiredAccess,
        uint dwShareMode,
        nint lpSecurityAttributes,
        uint dwCreationDisposition,
        uint dwFlagsAndAttributes,
        nint hTemplateFile);

    [DllImport("hid.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool HidD_GetAttributes(
        SafeFileHandle hidDeviceObject,
        ref HIDD_ATTRIBUTES attributes);

    [DllImport("hid.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool HidD_GetPreparsedData(
        SafeFileHandle hidDeviceObject,
        out nint preparsedData);

    [DllImport("hid.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool HidD_FreePreparsedData(nint preparsedData);

    [DllImport("hid.dll", SetLastError = true)]
    private static extern int HidP_GetCaps(
        nint preparsedData,
        out HIDP_CAPS capabilities);

    [DllImport("hid.dll", SetLastError = true)]
    private static extern int HidP_GetValueCaps(
        int reportType,
        [Out] HIDP_VALUE_CAPS[] valueCaps,
        ref ushort valueCapsLength,
        nint preparsedData);

    [DllImport("hid.dll", SetLastError = true)]
    private static extern int HidP_GetUsageValue(
        int reportType,
        ushort usagePage,
        ushort linkCollection,
        ushort usage,
        out uint usageValue,
        nint preparsedData,
        byte[] report,
        uint reportLength);

    [DllImport("hid.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool HidD_GetProductString(
        SafeFileHandle hidDeviceObject,
        byte[] buffer,
        uint bufferLength);

    [DllImport("hid.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool HidD_GetFeature(
        SafeFileHandle hidDeviceObject,
        byte[] reportBuffer,
        uint reportBufferLength);

    [DllImport("hid.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool HidD_SetFeature(
        SafeFileHandle hidDeviceObject,
        byte[] reportBuffer,
        uint reportBufferLength);

    [DllImport("hid.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool HidD_SetOutputReport(
        SafeFileHandle hidDeviceObject,
        byte[] reportBuffer,
        uint reportBufferLength);

    [DllImport("hid.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool HidD_GetInputReport(
        SafeFileHandle hidDeviceObject,
        byte[] reportBuffer,
        uint reportBufferLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WriteFile(
        SafeFileHandle hFile,
        nint lpBuffer,
        uint nNumberOfBytesToWrite,
        nint lpNumberOfBytesWritten,
        nint lpOverlapped);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ReadFile(
        SafeFileHandle hFile,
        nint lpBuffer,
        uint nNumberOfBytesToRead,
        nint lpNumberOfBytesRead,
        nint lpOverlapped);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern nint CreateEventW(
        nint lpEventAttributes,
        [MarshalAs(UnmanagedType.Bool)] bool bManualReset,
        [MarshalAs(UnmanagedType.Bool)] bool bInitialState,
        string? lpName);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WaitForSingleObject(
        nint hHandle,
        uint dwMilliseconds);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CancelIoEx(
        SafeFileHandle hFile,
        nint lpOverlapped);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetOverlappedResult(
        SafeFileHandle hFile,
        nint lpOverlapped,
        out uint lpNumberOfBytesTransferred,
        [MarshalAs(UnmanagedType.Bool)] bool bWait);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(nint hObject);
}
