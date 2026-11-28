#define UNICODE
#define _UNICODE
#include <windows.h>
#include <shellapi.h>
#include <shlobj.h>
#include <stdio.h>
#include <stdlib.h>
#include <wchar.h>

#define ABS_AUTOHIDE    0x0000001
#define ABS_ALWAYSONTOP 0x0000002

#define WATCHDOG_PATH_CAPACITY 1024
#define HEARTBEAT_TIMEOUT_MS 4000
#define HEARTBEAT_MISSES_REQUIRED 3
#define TOKEN_CAPACITY 128

// Journal write failures are logged by the Rust side; the watchdog only reads
// and deletes journals, but a failed delete is worth a debugger trace because
// it can cause a stale restore on the next crash.
static void DebugLogW(LPCWSTR message) {
    OutputDebugStringW(L"OpenTaskbarWatchdog: ");
    OutputDebugStringW(message);
    OutputDebugStringW(L"\n");
}

static void AttachToDefaultDesktop(void) {
    HDESK hDesk = OpenDesktopW(L"Default", 0, FALSE, GENERIC_ALL);
    if (hDesk) {
        SetThreadDesktop(hDesk);
        CloseDesktop(hDesk);
    }
}

static BOOL CALLBACK EnumSecondaryTrays(HWND hwnd, LPARAM lParam) {
    (void)lParam;
    WCHAR className[64];
    if (GetClassNameW(hwnd, className, 64) > 0) {
        if (wcscmp(className, L"Shell_SecondaryTrayWnd") == 0) {
            ShowWindow(hwnd, SW_SHOW);
            SetWindowPos(hwnd, NULL, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED);
        }
    }
    return TRUE;
}

typedef enum FlyoutRestoreStatus {
    FLYOUT_JOURNAL_CLEAN = 0,
    FLYOUT_JOURNAL_DEFERRED = 1,
    FLYOUT_JOURNAL_FOREIGN = 2
} FlyoutRestoreStatus;

static BOOL GetJournalPath(LPCWSTR fileName, WCHAR *path, DWORD capacity) {
    if (!fileName || !path || capacity == 0) return FALSE;
    // Resolve LocalAppData through the Windows Shell API rather than the
    // process environment block so a caller cannot redirect journal reads
    // or deletes by spoofing LOCALAPPDATA.
    WCHAR localAppData[MAX_PATH] = {0};
    if (FAILED(SHGetFolderPathW(NULL, CSIDL_LOCAL_APPDATA, NULL, SHGFP_TYPE_CURRENT, localAppData))) {
        return FALSE;
    }
    if (localAppData[0] == L'\0' || wcsstr(localAppData, L"..") != NULL) {
        return FALSE;
    }
    // Reject UNC paths and non-basename file names as defense-in-depth.
    if (localAppData[0] == L'\\' && localAppData[1] == L'\\') {
        return FALSE;
    }
    if (wcschr(fileName, L'\\') != NULL || wcschr(fileName, L'/') != NULL || wcsstr(fileName, L"..") != NULL) {
        return FALSE;
    }
    return _snwprintf_s(path, capacity, _TRUNCATE, L"%s\\OpenTaskbar\\%s", localAppData, fileName) >= 0;
}

// Returns Start/Search/Control Center to the positions recorded before the dock anchored them. The journal is
// atomic (written via flyout-positions.txt.tmp by BOTH the reference writer and Rust flyout_writer) and PID-checked
// so a recycled HWND can never be moved by a stale entry. The four scanned fields are the shared wire format
// "<hwnd> <pid> <left> <top>"; Rust may append generation/seq tokens, which fwscanf intentionally ignores.
// The journal is deleted later only when every entry was restored: a not-yet-created HWND (shell flyout host
// starts late) or a foreign owner keeps the only recovery clue for a later pass, matching flyout_writer.rs.
static FlyoutRestoreStatus RestoreFlyoutPositions(void) {
    WCHAR path[WATCHDOG_PATH_CAPACITY];
    if (!GetJournalPath(L"flyout-positions.txt", path, WATCHDOG_PATH_CAPACITY)) {
        return FLYOUT_JOURNAL_CLEAN;
    }
    FILE *file = _wfopen(path, L"r");
    if (!file) return FLYOUT_JOURNAL_CLEAN;

    FlyoutRestoreStatus status = FLYOUT_JOURNAL_CLEAN;
    unsigned long long handleValue = 0;
    unsigned int processId = 0;
    int left = 0;
    int top = 0;
    while (fwscanf(file, L"%llu %u %d %d", &handleValue, &processId, &left, &top) == 4) {
        HWND handle = (HWND)(UINT_PTR)handleValue;
        if (!IsWindow(handle)) {
            status = FLYOUT_JOURNAL_DEFERRED;
            continue;
        }

        DWORD actualProcessId = 0;
        GetWindowThreadProcessId(handle, &actualProcessId);
        if (actualProcessId != (DWORD)processId) {
            if (status != FLYOUT_JOURNAL_DEFERRED) status = FLYOUT_JOURNAL_FOREIGN;
            continue;
        }

        SetWindowPos(handle, NULL, left, top, 0, 0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
    fclose(file);
    return status;
}

// A killed tray host leaves app icons registered with its destroyed window; ask apps to re-register them.
static void BroadcastTaskbarCreated(void) {
    UINT message = RegisterWindowMessageW(L"TaskbarCreated");
    if (message) SendNotifyMessageW((HWND)0xFFFF, message, 0, 0);
}

static void RestoreNativeTaskbars(void) {
    AttachToDefaultDesktop();

    // 0. The dock may be gone, but its flyouts are hosted by long-lived shell processes. Put them back first.
    FlyoutRestoreStatus flyoutStatus = RestoreFlyoutPositions();
    WCHAR flyoutPath[WATCHDOG_PATH_CAPACITY] = {0};
    BOOL hasFlyoutJournal = GetJournalPath(L"flyout-positions.txt", flyoutPath, WATCHDOG_PATH_CAPACITY);

    // 1. Read journal if available to recover original auto-hide/always-on-top state. Keep it until the
    //    taskbar is actually visible; deleting it on an Explorer restart would destroy the only recovery clue.
    WCHAR journalPath[WATCHDOG_PATH_CAPACITY] = {0};
    BOOL hasJournal = GetJournalPath(L"taskbar-state.txt", journalPath, WATCHDOG_PATH_CAPACITY);
    DWORD state = 0;
    if (hasJournal) {
        FILE *jf = _wfopen(journalPath, L"r");
        if (jf) {
            unsigned int readVal = 0;
            if (fwscanf(jf, L"%u", &readVal) == 1) {
                state = (DWORD)readVal & (ABS_AUTOHIDE | ABS_ALWAYSONTOP);
            }
            fclose(jf);
        }
    }

    // 2. Locate primary taskbar
    HWND hPrimary = FindWindowW(L"Shell_TrayWnd", NULL);
    if (!hPrimary) return;

    // 3. Re-assert state and visibility with retries (defeats transient Explorer re-hides)
    for (int attempt = 0; attempt < 3; attempt++) {
        APPBARDATA abd;
        ZeroMemory(&abd, sizeof(abd));
        abd.cbSize = sizeof(APPBARDATA);
        abd.hWnd = hPrimary;
        abd.lParam = (LPARAM)state;
        SHAppBarMessage(ABM_SETSTATE, &abd);

        ShowWindow(hPrimary, SW_SHOW);
        SetWindowPos(hPrimary, NULL, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED);

        EnumWindows(EnumSecondaryTrays, 0);

        if (attempt < 2) Sleep(60);
    }

    // 4. Match the Rust path: two CONSECUTIVE visible observations 60 ms apart.
    //    Explorer may re-hide the tray right after we show it; one positive
    //    observation is not enough to delete the only recovery clue.
    int cleanPasses = 0;
    for (int attempt = 0; attempt < 3; attempt++) {
        if (IsWindowVisible(hPrimary)) {
            cleanPasses++;
            if (cleanPasses >= 2) break;
        } else {
            cleanPasses = 0;
            ShowWindow(hPrimary, SW_SHOW);
            EnumWindows(EnumSecondaryTrays, 0);
        }
        if (attempt < 2) Sleep(60);
    }
    if (cleanPasses < 2) return;

    if (hasJournal && DeleteFileW(journalPath) == 0 && GetLastError() != ERROR_FILE_NOT_FOUND) {
        DebugLogW(L"Could not delete taskbar-state recovery journal.");
    }
    if (hasFlyoutJournal && flyoutStatus == FLYOUT_JOURNAL_CLEAN
        && DeleteFileW(flyoutPath) == 0 && GetLastError() != ERROR_FILE_NOT_FOUND) {
        DebugLogW(L"Could not delete flyout position recovery journal.");
    }
    BroadcastTaskbarCreated();
}

static BOOL IsHexToken(LPCWSTR token) {
    size_t length = wcslen(token);
    if (length != 16 && length != 32) return FALSE;
    for (size_t i = 0; i < length; i++) {
        WCHAR c = token[i];
        if (!((c >= L'0' && c <= L'9') || (c >= L'a' && c <= L'f') || (c >= L'A' && c <= L'F'))) {
            return FALSE;
        }
    }
    return TRUE;
}

// Mirrors watchdog.rs parse_watchdog_arg: --watchdog=<decimal pid>:<16|32 hex token>.
// Rejecting malformed arguments prevents an injection path into the named-event
// and journal paths, and keeps C/Rust parsing on the same vector.
static BOOL ParseWatchdogArgument(int argc, LPWSTR *argv, int *parentPid, WCHAR *token, DWORD tokenCapacity) {
    *parentPid = 0;
    token[0] = L'\0';
    for (int i = 1; i < argc; i++) {
        if (wcsncmp(argv[i], L"--watchdog=", 11) != 0) continue;
        LPCWSTR data = argv[i] + 11;
        LPCWSTR colon = wcschr(data, L':');
        if (!colon || colon == data) continue;

        DWORD pid = 0;
        BOOL digitsOnly = TRUE;
        for (LPCWSTR p = data; p < colon; p++) {
            if (*p < L'0' || *p > L'9') { digitsOnly = FALSE; break; }
            DWORD digit = (DWORD)(*p - L'0');
            if (pid > (MAXDWORD - digit) / 10) { digitsOnly = FALSE; break; }
            pid = pid * 10 + digit;
        }
        LPCWSTR tokenText = colon + 1;
        if (!digitsOnly || pid == 0 || !IsHexToken(tokenText)) continue;
        if (wcslen(tokenText) >= tokenCapacity) continue;

        *parentPid = (int)pid;
        wcscpy_s(token, tokenCapacity, tokenText);
        return TRUE;
    }
    return FALSE;
}

int WINAPI WinMain(HINSTANCE hInstance, HINSTANCE hPrevInstance, LPSTR lpCmdLine, int nCmdShow) {
    (void)hInstance;
    (void)hPrevInstance;
    (void)lpCmdLine;
    (void)nCmdShow;

    AttachToDefaultDesktop();

    int argc = 0;
    LPWSTR *argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    if (!argv) return 1;
    int parentPid = 0;
    WCHAR token[TOKEN_CAPACITY] = {0};
    BOOL argsValid = ParseWatchdogArgument(argc, argv, &parentPid, token, TOKEN_CAPACITY);
    LocalFree(argv);
    if (!argsValid) return 1;

    HANDLE hProcess = OpenProcess(SYNCHRONIZE, FALSE, (DWORD)parentPid);
    if (!hProcess) {
        RestoreNativeTaskbars();
        return 0;
    }

    WCHAR eventName[256];
    if (_snwprintf_s(eventName, 256, _TRUNCATE, L"Local\\OpenTaskbar.CleanExit.%d.%s", parentPid, token) < 0) {
        DebugLogW(L"Clean-exit event name was truncated.");
        CloseHandle(hProcess);
        return 1;
    }
    HANDLE hCleanExit = OpenEventW(SYNCHRONIZE, FALSE, eventName);
    if (!hCleanExit) DebugLogW(L"Clean-exit event could not be opened; fail-closed on restore.");

    WCHAR heartbeatName[256];
    HANDLE hHeartbeat = NULL;
    if (_snwprintf_s(heartbeatName, 256, _TRUNCATE, L"Local\\OpenTaskbar.Heartbeat.%d.%s", parentPid, token) >= 0) {
        hHeartbeat = OpenEventW(SYNCHRONIZE, FALSE, heartbeatName);
    } else {
        DebugLogW(L"Heartbeat event name was truncated.");
    }

    // No `(DWORD)-1` index sentinels: a missing handle is never a wait index,
    // so WAIT_FAILED can never alias "clean exit" (0 + MAXDWORD).
    HANDLE waitHandles[3];
    DWORD waitCount = 0;
    if (hCleanExit) waitHandles[waitCount++] = hCleanExit;
    waitHandles[waitCount++] = hProcess;
    if (hHeartbeat) waitHandles[waitCount++] = hHeartbeat;

    BOOL cleanExit = FALSE;
    int heartbeatMisses = 0;
    for (;;) {
        DWORD waitResult = WaitForMultipleObjects(
            waitCount, waitHandles, FALSE, hHeartbeat ? HEARTBEAT_TIMEOUT_MS : INFINITE);

        // Classify errors and abandoned handles as "not clean": restore instead
        // of trusting an ambiguous wait result.
        if (waitResult == WAIT_FAILED) {
            DebugLogW(L"Wait failed; restoring taskbar (fail closed).");
            break;
        }
        if (waitResult >= WAIT_ABANDONED_0 && waitResult < WAIT_ABANDONED_0 + waitCount) {
            DebugLogW(L"Wait abandoned; restoring taskbar (fail closed).");
            break;
        }
        if (waitResult >= WAIT_OBJECT_0 && waitResult < WAIT_OBJECT_0 + waitCount) {
            HANDLE signalled = waitHandles[waitResult - WAIT_OBJECT_0];
            if (signalled == hProcess) {
                // Process death: only a previously signalled clean-exit event
                // makes this a clean shutdown.
                cleanExit = hCleanExit && WaitForSingleObject(hCleanExit, 0) == WAIT_OBJECT_0;
                break;
            }
            if (signalled == hCleanExit) {
                cleanExit = TRUE;
                break;
            }
            if (signalled == hHeartbeat) {
                heartbeatMisses = 0;
                continue;
            }
        }
        if (waitResult == WAIT_TIMEOUT) {
            if (hHeartbeat && ++heartbeatMisses >= HEARTBEAT_MISSES_REQUIRED) break;
            continue;
        }
        break;
    }

    if (hHeartbeat) CloseHandle(hHeartbeat);
    if (hCleanExit) CloseHandle(hCleanExit);
    CloseHandle(hProcess);

    if (!cleanExit) {
        RestoreNativeTaskbars();
    }

    return 0;
}
