/*
 * The Windows Installer package's check for a running Spotifast
 * (packaging/windows/spotifast.wxs runs CheckRunningCopy before
 * InstallValidate, so before anything is replaced or removed).
 *
 * Windows keeps a running program's file. Removing Spotifast while it runs
 * would remove everything else and leave the program behind (#766), and an
 * update would only finish after a restart. The check opens the installed
 * spotifast.exe for writing, which Windows refuses while it runs, and then:
 *
 * - with a user interface, asks: Yes closes the installed copy, No checks
 *   again once the user has quit it, Cancel stops;
 * - without one, stops an uninstall before anything is removed, and closes
 *   the installed copy before an install or update replaces it. The updater
 *   quits Spotifast itself before it runs the package.
 *
 * Only processes running that exact file are closed, so a portable copy with
 * the same name elsewhere keeps running and never blocks anything.
 *
 * build-msi.py compiles it with the MSVC compiler for the package's
 * architecture and embeds it in the package; it never ships on its own.
 */

#define WIN32_LEAN_AND_MEAN
#ifndef UNICODE
#define UNICODE
#endif
/* Windows 10, as the package requires: FileIdInfo needs 8 or later. */
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif
#include <windows.h>
#include <msi.h>
#include <msiquery.h>
#include <tlhelp32.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

#define EXECUTABLE L"spotifast.exe"

/* Answers to one check before it gives up: a user interface that answers
   on its own must not keep it asking forever. */
#define MAX_PROMPTS 100

/* WixUI's ErrorDlg has room for about seven short lines; keep to five. */
static const wchar_t REMOVE_QUESTION[] =
    L"Spotifast is running, so it cannot be removed yet.\r\n\r\n"
    L"Close it now? Yes ends it immediately; unsaved changes may be lost. "
    L"No checks again after you quit it. Cancel keeps it installed.";

static const wchar_t UPDATE_QUESTION[] =
    L"Spotifast is running, so it cannot be updated yet.\r\n\r\n"
    L"Close it now? Yes ends it immediately; unsaved changes may be lost. "
    L"No checks again after you quit it. Cancel stops the setup.";

enum state { STATE_MISSING, STATE_FREE, STATE_IN_USE };

struct identity {
    BOOL wide;
    FILE_ID_INFO id;
    BY_HANDLE_FILE_INFORMATION info;
};

static void *allocate(SIZE_T bytes)
{
    return HeapAlloc(GetProcessHeap(), 0, bytes);
}

static void release(void *memory)
{
    if (memory) {
        HeapFree(GetProcessHeap(), 0, memory);
    }
}

/* A property's value in a new buffer, or NULL. */
static wchar_t *property(MSIHANDLE install, const wchar_t *name)
{
    wchar_t empty[1] = L"";
    DWORD length = 0;
    UINT result = MsiGetPropertyW(install, name, empty, &length);
    if (result != ERROR_MORE_DATA && result != ERROR_SUCCESS) {
        return NULL;
    }
    length += 1;
    wchar_t *value = allocate(length * sizeof *value);
    if (value && MsiGetPropertyW(install, name, value, &length) != ERROR_SUCCESS) {
        release(value);
        value = NULL;
    }
    return value;
}

/* Writes a line to the installer's log (msiexec /l*v). */
static void note(MSIHANDLE install, const wchar_t *text, const wchar_t *path)
{
    MSIHANDLE record = MsiCreateRecord(2);
    if (!record) {
        return;
    }
    MsiRecordSetStringW(record, 0, L"CheckRunningCopy: [1] [2]");
    MsiRecordSetStringW(record, 1, text);
    MsiRecordSetStringW(record, 2, path ? path : L"");
    MsiProcessMessage(install, INSTALLMESSAGE_INFO, record);
    MsiCloseHandle(record);
}

/* The installed executable: the installation folder and EXECUTABLE. */
static wchar_t *executable_in(const wchar_t *folder)
{
    size_t folder_length = wcslen(folder);
    size_t name_length = wcslen(EXECUTABLE);
    BOOL separator = folder_length > 0 && folder[folder_length - 1] != L'\\';
    size_t length = folder_length + (separator ? 1 : 0) + name_length;
    wchar_t *path = allocate((length + 1) * sizeof *path);
    if (!path) {
        return NULL;
    }
    memcpy(path, folder, folder_length * sizeof *path);
    if (separator) {
        path[folder_length] = L'\\';
    }
    memcpy(path + length - name_length, EXECUTABLE, name_length * sizeof *path);
    path[length] = L'\0';
    return path;
}

/* Windows refuses write access to a running program's file. Nothing is
   written: opening it also catches other locks on this exact file. A few
   tries let a virus scanner's brief look pass. */
static enum state probe(const wchar_t *path)
{
    for (int attempt = 1;; attempt++) {
        HANDLE file = CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 0, NULL,
                                  OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
        if (file != INVALID_HANDLE_VALUE) {
            CloseHandle(file);
            return STATE_FREE;
        }
        DWORD error = GetLastError();
        if (error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND) {
            return STATE_MISSING;
        }
        if (attempt == 5) {
            return STATE_IN_USE;
        }
        Sleep(100);
    }
}

/* Which file a path names, whatever its spelling: case, short names and
   links all lead to the same file ID. */
static BOOL identify(const wchar_t *path, struct identity *identity)
{
    HANDLE file = CreateFileW(path, FILE_READ_ATTRIBUTES,
                              FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                              NULL, OPEN_EXISTING, 0, NULL);
    if (file == INVALID_HANDLE_VALUE) {
        return FALSE;
    }
    ZeroMemory(identity, sizeof *identity);
    identity->wide = GetFileInformationByHandleEx(file, FileIdInfo, &identity->id,
                                                  sizeof identity->id);
    BOOL known = identity->wide || GetFileInformationByHandle(file, &identity->info);
    CloseHandle(file);
    return known;
}

static BOOL same_file(const struct identity *a, const struct identity *b)
{
    if (a->wide && b->wide) {
        return a->id.VolumeSerialNumber == b->id.VolumeSerialNumber
            && memcmp(&a->id.FileId, &b->id.FileId, sizeof a->id.FileId) == 0;
    }
    if (a->wide || b->wide) {
        return FALSE;
    }
    return a->info.dwVolumeSerialNumber == b->info.dwVolumeSerialNumber
        && a->info.nFileIndexHigh == b->info.nFileIndexHigh
        && a->info.nFileIndexLow == b->info.nFileIndexLow;
}

/* Ends every process running the file at `path`, and waits for each to go.
   Processes with the same name that run another file are left alone. */
static void close_installed_copy(MSIHANDLE install, const wchar_t *path)
{
    struct identity installed;
    if (!identify(path, &installed)) {
        note(install, L"could not identify", path);
        return;
    }
    const wchar_t *name = wcsrchr(path, L'\\');
    name = name ? name + 1 : path;
    const DWORD capacity = 32768;
    wchar_t *image = allocate(capacity * sizeof *image);
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (!image || snapshot == INVALID_HANDLE_VALUE) {
        note(install, L"could not list the processes running", path);
        release(image);
        if (snapshot != INVALID_HANDLE_VALUE) {
            CloseHandle(snapshot);
        }
        return;
    }
    PROCESSENTRY32W entry;
    entry.dwSize = sizeof entry;
    for (BOOL more = Process32FirstW(snapshot, &entry); more;
         more = Process32NextW(snapshot, &entry)) {
        if (entry.th32ProcessID == GetCurrentProcessId()
            || CompareStringOrdinal(entry.szExeFile, -1, name, -1, TRUE) != CSTR_EQUAL) {
            continue;
        }
        HANDLE process = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | SYNCHRONIZE, FALSE,
            entry.th32ProcessID);
        if (!process) {
            continue;
        }
        DWORD length = capacity;
        struct identity running;
        if (QueryFullProcessImageNameW(process, 0, image, &length)
            && identify(image, &running) && same_file(&installed, &running)) {
            if (TerminateProcess(process, 1)) {
                WaitForSingleObject(process, 10000);
                note(install, L"closed the copy running from", path);
            } else {
                note(install, L"could not close the copy running from", path);
            }
        }
        CloseHandle(process);
    }
    CloseHandle(snapshot);
    release(image);
}

static int ask(MSIHANDLE install, BOOL removing)
{
    MSIHANDLE record = MsiCreateRecord(0);
    if (!record) {
        return 0;
    }
    MsiRecordSetStringW(record, 0, removing ? REMOVE_QUESTION : UPDATE_QUESTION);
    int answer = MsiProcessMessage(
        install,
        (INSTALLMESSAGE)(INSTALLMESSAGE_USER | MB_YESNOCANCEL | MB_ICONWARNING | MB_DEFBUTTON3),
        record);
    MsiCloseHandle(record);
    return answer;
}

__declspec(dllexport) UINT __stdcall CheckRunningCopy(MSIHANDLE install)
{
    UINT result = ERROR_SUCCESS;
    wchar_t *folder = property(install, L"INSTALLFOLDER");
    wchar_t *remove = property(install, L"REMOVE");
    wchar_t *level = property(install, L"UILevel");
    wchar_t *path = folder && *folder ? executable_in(folder) : NULL;
    BOOL removing = remove && wcscmp(remove, L"ALL") == 0;
    /* 2 is INSTALLUILEVEL_NONE: msiexec /qn, as the updater runs it. */
    BOOL interactive = level && _wtoi(level) > 2;

    if (!path) {
        note(install, L"no installation folder", NULL);
        goto done;
    }
    for (int prompts = 0;;) {
        enum state state = probe(path);
        if (state != STATE_IN_USE) {
            note(install, state == STATE_MISSING ? L"no program at" : L"not running from", path);
            break;
        }
        int answer = 0;
        if (interactive) {
            if (prompts == MAX_PROMPTS) {
                note(install, L"gave up asking, still running from", path);
                result = ERROR_INSTALL_USEREXIT;
                break;
            }
            prompts++;
            answer = ask(install, removing);
        }
        if (answer == IDCANCEL) {
            note(install, L"cancelled, still running from", path);
            result = ERROR_INSTALL_USEREXIT;
            break;
        }
        if (answer == IDYES) {
            close_installed_copy(install, path);
            continue;
        }
        if (answer == IDNO) {
            continue;
        }
        /* Nobody to ask. */
        if (removing) {
            note(install, L"stopped the uninstall, running from", path);
            result = ERROR_INSTALL_FAILURE;
            break;
        }
        note(install, L"closing it before the update, running from", path);
        close_installed_copy(install, path);
        /* Windows Installer handles whatever still holds the file. */
        break;
    }
done:
    release(path);
    release(level);
    release(remove);
    release(folder);
    return result;
}
