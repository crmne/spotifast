/*
 * The Windows Installer package's removal of what the updater leaves in the
 * installation folder (packaging/windows/spotifast.wxs runs
 * RemoveUpdateLeftovers just before RemoveFiles, only when Spotifast itself
 * is being uninstalled).
 *
 * fastframe-update stages each update in a .spotifast-update-<suffix> folder
 * inside the installation folder: its helper, which runs the package, and a
 * copy of the previous version. It sweeps finished ones itself, but the last
 * one stays until the next update, and Windows Installer only removes the
 * files it installed. Without this, uninstalling would leave the folder
 * behind with those in it.
 *
 * Only folders directly inside the installation folder whose names start
 * with that prefix go; anything else there is left alone. A link (junction
 * or symbolic link) is removed itself, never followed. Whatever cannot be
 * removed, such as a file another program holds, stays, and the uninstall
 * goes on: these are leftovers, never worth failing it for.
 *
 * build-msi.py compiles it with running-copy.c into the package's custom
 * action library; it never ships on its own.
 */

#define WIN32_LEAN_AND_MEAN
#ifndef UNICODE
#define UNICODE
#endif
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif
#include <windows.h>
#include <msi.h>
#include <msiquery.h>
#include <string.h>
#include <wchar.h>

#define PREFIX L".spotifast-update-"

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

/* Writes a line to the installer's log (msiexec /l*v). */
static void note(MSIHANDLE install, const wchar_t *text, const wchar_t *path)
{
    MSIHANDLE record = MsiCreateRecord(2);
    if (!record) {
        return;
    }
    MsiRecordSetStringW(record, 0, L"RemoveUpdateLeftovers: [1] [2]");
    MsiRecordSetStringW(record, 1, text);
    MsiRecordSetStringW(record, 2, path ? path : L"");
    MsiProcessMessage(install, INSTALLMESSAGE_INFO, record);
    MsiCloseHandle(record);
}

/* `folder`, a backslash unless it ends with one, and `name`, in a new buffer. */
static wchar_t *join(const wchar_t *folder, const wchar_t *name)
{
    size_t folder_length = wcslen(folder);
    size_t name_length = wcslen(name);
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
    memcpy(path + length - name_length, name, name_length * sizeof *path);
    path[length] = L'\0';
    return path;
}

static BOOL dots(const wchar_t *name)
{
    return wcscmp(name, L".") == 0 || wcscmp(name, L"..") == 0;
}

static BOOL is_link(DWORD attributes)
{
    return (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
}

static void remove_file(MSIHANDLE install, const wchar_t *path, DWORD attributes)
{
    if (attributes & FILE_ATTRIBUTE_READONLY) {
        SetFileAttributesW(path, attributes & ~FILE_ATTRIBUTE_READONLY);
    }
    if (!DeleteFileW(path)) {
        note(install, L"could not remove", path);
    }
}

/* Removes `path`, a folder, with everything in it; a link only itself. */
static void remove_tree(MSIHANDLE install, const wchar_t *path, DWORD attributes, int depth)
{
    if (attributes & FILE_ATTRIBUTE_READONLY) {
        SetFileAttributesW(path, attributes & ~FILE_ATTRIBUTE_READONLY);
    }
    if (!is_link(attributes) && depth < 64) {
        wchar_t *pattern = join(path, L"*");
        WIN32_FIND_DATAW entry = {0};
        HANDLE find = pattern ? FindFirstFileExW(pattern, FindExInfoBasic, &entry,
                                                 FindExSearchNameMatch, NULL, 0)
                              : INVALID_HANDLE_VALUE;
        if (find != INVALID_HANDLE_VALUE) {
            do {
                if (dots(entry.cFileName)) {
                    continue;
                }
                wchar_t *child = join(path, entry.cFileName);
                if (!child) {
                    continue;
                }
                if (entry.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
                    remove_tree(install, child, entry.dwFileAttributes, depth + 1);
                } else {
                    remove_file(install, child, entry.dwFileAttributes);
                }
                release(child);
            } while (FindNextFileW(find, &entry));
            FindClose(find);
        }
        release(pattern);
    }
    if (!RemoveDirectoryW(path)) {
        note(install, L"could not remove", path);
    }
}

__declspec(dllexport) UINT __stdcall RemoveUpdateLeftovers(MSIHANDLE install)
{
    /* Deferred: the installation folder comes as CustomActionData. */
    wchar_t empty[1] = L"";
    DWORD length = 0;
    wchar_t *folder = NULL;
    UINT result = MsiGetPropertyW(install, L"CustomActionData", empty, &length);
    if (result == ERROR_MORE_DATA || result == ERROR_SUCCESS) {
        length += 1;
        folder = allocate(length * sizeof *folder);
        if (folder && MsiGetPropertyW(install, L"CustomActionData", folder, &length) != ERROR_SUCCESS) {
            release(folder);
            folder = NULL;
        }
    }
    /* Only a full path (C:\... or \\server\...): never a folder relative to
       wherever msiexec runs. */
    BOOL full = folder && wcslen(folder) >= 3
        && ((folder[1] == L':' && folder[2] == L'\\') || (folder[0] == L'\\' && folder[1] == L'\\'));
    if (!full) {
        note(install, L"no installation folder", folder);
        release(folder);
        return ERROR_SUCCESS;
    }
    wchar_t *pattern = join(folder, PREFIX L"*");
    WIN32_FIND_DATAW entry = {0};
    HANDLE find = pattern ? FindFirstFileExW(pattern, FindExInfoBasic, &entry,
                                             FindExSearchNameMatch, NULL, 0)
                          : INVALID_HANDLE_VALUE;
    if (find != INVALID_HANDLE_VALUE) {
        size_t prefix = wcslen(PREFIX);
        do {
            /* The long name, as the updater wrote it: a short name can match
               a pattern its long name does not. */
            if (!(entry.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)
                || wcslen(entry.cFileName) <= prefix
                || CompareStringOrdinal(entry.cFileName, (int)prefix, PREFIX, (int)prefix, TRUE)
                       != CSTR_EQUAL) {
                continue;
            }
            wchar_t *path = join(folder, entry.cFileName);
            if (path) {
                note(install, L"removing", path);
                remove_tree(install, path, entry.dwFileAttributes, 0);
                release(path);
            }
        } while (FindNextFileW(find, &entry));
        FindClose(find);
    } else {
        note(install, L"nothing to remove in", folder);
    }
    release(pattern);
    release(folder);
    return ERROR_SUCCESS;
}
