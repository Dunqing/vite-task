use std::mem::{MaybeUninit, offset_of, size_of, size_of_val};

use fspy_shared::ipc::{AccessMode, IpcPath, PathAccess};
use ntapi::{
    ntioapi::{
        FILE_BASIC_INFORMATION, FILE_CREATED, FILE_DIRECTORY_FILE, FILE_INFORMATION_CLASS,
        FILE_OPEN, FILE_OVERWRITTEN, FILE_RENAME_INFORMATION, FILE_SUPERSEDED, FileLinkInformation,
        FileLinkInformationEx, FileRenameInformation, FileRenameInformationEx,
        NtQueryDirectoryFile, NtQueryFullAttributesFile, NtQueryInformationByName,
        PFILE_BASIC_INFORMATION, PFILE_NETWORK_OPEN_INFORMATION, PIO_APC_ROUTINE, PIO_STATUS_BLOCK,
    },
    ntpsapi::{
        NtCreateUserProcess, PPS_ATTRIBUTE_LIST, PPS_CREATE_INFO, PS_ATTRIBUTE,
        PS_ATTRIBUTE_IMAGE_NAME, PS_ATTRIBUTE_LIST,
    },
};
use winapi::{
    shared::{
        minwindef::HFILE,
        ntdef::{
            BOOLEAN, HANDLE, NT_SUCCESS, NTSTATUS, OBJECT_ATTRIBUTES, PHANDLE, PLARGE_INTEGER,
            POBJECT_ATTRIBUTES, PUNICODE_STRING, PVOID, ULONG, UNICODE_STRING,
        },
        ntstatus::{STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND},
    },
    um::winnt::{ACCESS_MASK, FILE_READ_DATA, GENERIC_READ},
};

use crate::windows::{
    client::global_client,
    convert::{ToAbsolutePath, ToAccessMode},
    detour::{Detour, DetourAny},
    winapi_utils::access_mask_to_mode,
};

// CreateProcess ultimately asks NtCreateUserProcess to open the executable image. Some Windows
// versions perform that open entirely inside the syscall, so it never reaches the NtCreateFile and
// query functions hooked below. PS_ATTRIBUTE_IMAGE_NAME is the kernel-facing path that identifies
// the image to open; RTL_USER_PROCESS_PARAMETERS.ImagePathName is only metadata for the child PEB
// and can intentionally name a different file.
//
// Record the image before forwarding the syscall, matching the attempted-access semantics of the
// other NT hooks in this module. This is important for missing executables: the failed lookup is
// still an input access even though NtCreateUserProcess returns an error.
static DETOUR_NT_CREATE_USER_PROCESS: Detour<
    unsafe extern "system" fn(
        process_handle: PHANDLE,
        thread_handle: PHANDLE,
        process_desired_access: ACCESS_MASK,
        thread_desired_access: ACCESS_MASK,
        process_object_attributes: POBJECT_ATTRIBUTES,
        thread_object_attributes: POBJECT_ATTRIBUTES,
        process_flags: ULONG,
        thread_flags: ULONG,
        process_parameters: PVOID,
        create_info: PPS_CREATE_INFO,
        attribute_list: PPS_ATTRIBUTE_LIST,
    ) -> NTSTATUS,
> =
    // SAFETY: initializing Detour with the real NtCreateUserProcess function pointer
    unsafe {
        Detour::new(c"NtCreateUserProcess", NtCreateUserProcess, {
            unsafe extern "system" fn new_fn(
                process_handle: PHANDLE,
                thread_handle: PHANDLE,
                process_desired_access: ACCESS_MASK,
                thread_desired_access: ACCESS_MASK,
                process_object_attributes: POBJECT_ATTRIBUTES,
                thread_object_attributes: POBJECT_ATTRIBUTES,
                process_flags: ULONG,
                thread_flags: ULONG,
                process_parameters: PVOID,
                create_info: PPS_CREATE_INFO,
                attribute_list: PPS_ATTRIBUTE_LIST,
            ) -> NTSTATUS {
                // SAFETY: observing caller memory without changing the forwarded arguments
                unsafe { handle_process_image(attribute_list) };

                // SAFETY: calling the original NtCreateUserProcess with all original arguments
                unsafe {
                    (DETOUR_NT_CREATE_USER_PROCESS.real())(
                        process_handle,
                        thread_handle,
                        process_desired_access,
                        thread_desired_access,
                        process_object_attributes,
                        thread_object_attributes,
                        process_flags,
                        thread_flags,
                        process_parameters,
                        create_info,
                        attribute_list,
                    )
                }
            }
            new_fn
        })
    };

unsafe fn handle_process_image(attribute_list: PPS_ATTRIBUTE_LIST) {
    // SAFETY: NtCreateUserProcess requires its attribute list to remain valid for this call.
    if let Some(image_path) = unsafe { read_process_image_attribute(attribute_list) } {
        // Sender serialization completes before this call returns, so IpcPath does not retain
        // the borrowed PS_ATTRIBUTE_IMAGE_NAME buffer past the NtCreateUserProcess call.
        // SAFETY: accessing the global client which was initialized during DLL_PROCESS_ATTACH
        unsafe { global_client() }.send(PathAccess::Read(IpcPath::from_wide(image_path)));
    }
}

/// Find the kernel-facing executable name in an `NtCreateUserProcess` attribute list.
///
/// `PS_ATTRIBUTE_LIST` is a variable-length structure: `TotalLength` covers a fixed-size header
/// followed by contiguous `PS_ATTRIBUTE` entries. Its Rust definition contains one placeholder
/// element, so the actual entry count must be derived from `TotalLength`, not from the array type.
///
/// The returned slice borrows the caller's `PS_ATTRIBUTE_IMAGE_NAME` buffer and is valid only while
/// the intercepted `NtCreateUserProcess` call is active.
///
/// # Safety
///
/// `attribute_list` and the image-name buffer it references must remain valid for the duration of
/// the intercepted call, as required by `NtCreateUserProcess`.
unsafe fn read_process_image_attribute<'a>(
    attribute_list: PPS_ATTRIBUTE_LIST,
) -> Option<&'a [u16]> {
    // SAFETY: NtCreateUserProcess keeps a supplied attribute list valid for this call; a null
    // optional pointer is parsed as None.
    let attribute_list = unsafe { attribute_list.as_ref()? };

    // Attributes is a trailing array. Subtract its byte offset to remove the header; the native API
    // contract guarantees that the remaining bytes contain complete PS_ATTRIBUTE entries.
    let attributes_offset = offset_of!(PS_ATTRIBUTE_LIST, Attributes);
    let attribute_count =
        (attribute_list.TotalLength - attributes_offset) / size_of::<PS_ATTRIBUTE>();

    // The Rust field exposes the first placeholder entry as a reference; TotalLength describes how
    // many contiguous entries follow it in the actual variable-length allocation.
    let first_attribute = attribute_list.Attributes.first()?;
    // SAFETY: TotalLength covers a contiguous variable-length tail starting at first_attribute.
    let attributes: &[PS_ATTRIBUTE] =
        unsafe { std::slice::from_raw_parts(std::ptr::from_ref(first_attribute), attribute_count) };
    for attribute in attributes {
        if attribute.Attribute != PS_ATTRIBUTE_IMAGE_NAME {
            continue;
        }

        // Unlike a UNICODE_STRING, PS_ATTRIBUTE_IMAGE_NAME stores the path buffer directly in
        // ValuePtr and stores its byte length in Size. It is the image path consumed by the kernel,
        // so do not fall back to the separately spoofable process-parameter path.
        // SAFETY: PS_ATTRIBUTE_IMAGE_NAME stores a valid UTF-16 pointer in ValuePtr for this call;
        // a null pointer is parsed as None.
        let image_path = unsafe { attribute.u.ValuePtr.cast::<u16>().as_ref()? };
        // SAFETY: the attribute contract guarantees a valid UTF-16 buffer of Size bytes for this
        // call. Size is the counted string length, so no NUL-terminator parsing is needed.
        return Some(unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(image_path),
                attribute.Size / size_of::<u16>(),
            )
        });
    }

    None
}

// A disposition other than `FILE_OPEN` can create, overwrite or supersede the
// file, which is a write whatever access the caller asked for: `CreateDirectoryW`
// and `CreateSymbolicLinkW` open with no write access, and `OPEN_ALWAYS` may
// only end up reading. What happened is known after the call, from the status
// block, so those opens are recorded then. A plain open is recorded before the
// call, like the other NT hooks.
static DETOUR_NT_CREATE_FILE: Detour<
    unsafe extern "system" fn(
        file_handle: PHANDLE,
        desired_access: ACCESS_MASK,
        object_attributes: POBJECT_ATTRIBUTES,
        io_status_block: PIO_STATUS_BLOCK,
        allocation_size: PLARGE_INTEGER,
        file_attributes: ULONG,
        share_access: ULONG,
        create_disposition: ULONG,
        create_options: ULONG,
        ea_buffer: PVOID,
        ea_length: ULONG,
    ) -> HFILE,
> =
    // SAFETY: initializing Detour with the real NtCreateFile function pointer and our replacement
    unsafe {
        Detour::new(c"NtCreateFile", ntapi::ntioapi::NtCreateFile, {
            unsafe extern "system" fn new_nt_create_file(
                file_handle: PHANDLE,
                desired_access: ACCESS_MASK,
                object_attributes: POBJECT_ATTRIBUTES,
                io_status_block: PIO_STATUS_BLOCK,
                allocation_size: PLARGE_INTEGER,
                file_attributes: ULONG,
                share_access: ULONG,
                create_disposition: ULONG,
                create_options: ULONG,
                ea_buffer: PVOID,
                ea_length: ULONG,
            ) -> HFILE {
                if create_disposition == FILE_OPEN {
                    // SAFETY: intercepting file open to record access before forwarding to real function
                    unsafe { handle_open(desired_access, false, object_attributes) };
                }

                // SAFETY: calling the original NtCreateFile with all original arguments
                let status = unsafe {
                    (DETOUR_NT_CREATE_FILE.real())(
                        file_handle,
                        desired_access,
                        object_attributes,
                        io_status_block,
                        allocation_size,
                        file_attributes,
                        share_access,
                        create_disposition,
                        create_options,
                        ea_buffer,
                        ea_length,
                    )
                };

                if create_disposition != FILE_OPEN {
                    let information = if NT_SUCCESS(status) {
                        // SAFETY: the status block is the caller's and was filled by
                        // the call, which succeeded.
                        let information = unsafe { (*io_status_block).Information };
                        Some(u32::try_from(information).unwrap_or(u32::MAX))
                    } else {
                        None
                    };
                    let (mode, created) =
                        creating_open_access(desired_access, create_options, information);
                    // SAFETY: recording the open from the caller's object attributes,
                    // which outlive the call
                    unsafe { handle_open(mode, created, object_attributes) };
                }
                status
            }
            new_nt_create_file
        })
    };

/// The access an open with a creating disposition made, from the status
/// block's `Information` after the call (`None` when the call failed).
///
/// The file was written when it was created, overwritten or superseded, and
/// a failed attempt counts as the write it tried to make. An existing file
/// that was merely opened saw only the requested access. A directory's
/// `FILE_LIST_DIRECTORY` right is not a read of its contents: listing it is
/// recorded by `NtQueryDirectoryFile`, so creating a directory is a plain
/// write, as `mkdir` is on Unix.
const fn creating_open_access(
    desired_access: ACCESS_MASK,
    create_options: ULONG,
    information: Option<ULONG>,
) -> (AccessMode, bool) {
    let read = desired_access & (FILE_READ_DATA | GENERIC_READ) != 0
        && create_options & FILE_DIRECTORY_FILE == 0;
    let wrote = if read { AccessMode::ReadWrite } else { AccessMode::Write };
    match information {
        Some(FILE_CREATED) => (wrote, true),
        Some(FILE_OVERWRITTEN | FILE_SUPERSEDED) | None => (wrote, false),
        Some(_) => (access_mask_to_mode(desired_access), false),
    }
}

// Rename and link destinations. `MoveFileExW`, `CreateHardLinkW` and their
// `ReplaceFile` relatives all end here. The destination is recorded as a
// write before the call, created when nothing is there yet; the source needs
// no record.
static DETOUR_NT_SET_INFORMATION_FILE: Detour<
    unsafe extern "system" fn(
        file_handle: HANDLE,
        io_status_block: PIO_STATUS_BLOCK,
        file_information: PVOID,
        length: ULONG,
        file_information_class: FILE_INFORMATION_CLASS,
    ) -> NTSTATUS,
> =
    // SAFETY: initializing Detour with the real NtSetInformationFile function pointer
    unsafe {
        Detour::new(c"NtSetInformationFile", ntapi::ntioapi::NtSetInformationFile, {
            unsafe extern "system" fn new_fn(
                file_handle: HANDLE,
                io_status_block: PIO_STATUS_BLOCK,
                file_information: PVOID,
                length: ULONG,
                file_information_class: FILE_INFORMATION_CLASS,
            ) -> NTSTATUS {
                if [
                    FileRenameInformation,
                    FileRenameInformationEx,
                    FileLinkInformation,
                    FileLinkInformationEx,
                ]
                .contains(&file_information_class)
                {
                    // SAFETY: the information buffer is the caller's and is valid for
                    // `length` bytes during this call.
                    unsafe { handle_rename_destination(file_information, length) };
                }
                // SAFETY: calling the original NtSetInformationFile with all original arguments
                unsafe {
                    (DETOUR_NT_SET_INFORMATION_FILE.real())(
                        file_handle,
                        io_status_block,
                        file_information,
                        length,
                        file_information_class,
                    )
                }
            }
            new_fn
        })
    };

/// Records the destination of a rename or link from its `FILE_RENAME_INFORMATION`
/// (`FILE_LINK_INFORMATION` has the same layout): a name relative to
/// `RootDirectory`, or a full NT path when that handle is null.
///
/// # Safety
///
/// `file_information` must be valid for `length` bytes for the duration of
/// the intercepted call.
unsafe fn handle_rename_destination(file_information: PVOID, length: ULONG) {
    #[expect(clippy::cast_possible_truncation, reason = "the structure is a few dozen bytes")]
    const OBJECT_ATTRIBUTES_LENGTH: ULONG = size_of::<OBJECT_ATTRIBUTES>() as ULONG;

    let name_offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
    let Ok(length) = usize::try_from(length) else {
        return;
    };
    if file_information.is_null() || length < name_offset {
        return;
    }
    let info = file_information.cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: the buffer holds at least the fixed part of the structure.
    let (root_directory, name_length) =
        unsafe { ((*info).RootDirectory, usize::try_from((*info).FileNameLength).unwrap_or(0)) };
    let name_length = name_length.min(length - name_offset);
    // SAFETY: `FileName` is the trailing array; `name_length` bytes of it are
    // within the buffer.
    let file_name = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*info).FileName).cast::<u16>(),
            name_length / size_of::<u16>(),
        )
    };
    let Ok(name_bytes) = u16::try_from(size_of_val(file_name)) else {
        return;
    };

    let mut object_name = UNICODE_STRING {
        Length: name_bytes,
        MaximumLength: name_bytes,
        Buffer: file_name.as_ptr().cast_mut(),
    };
    let mut object_attributes = OBJECT_ATTRIBUTES {
        Length: OBJECT_ATTRIBUTES_LENGTH,
        RootDirectory: root_directory,
        ObjectName: &raw mut object_name,
        Attributes: 0,
        SecurityDescriptor: std::ptr::null_mut(),
        SecurityQualityOfService: std::ptr::null_mut(),
    };

    // Through the real function: the hooked one would record a read.
    let mut basic_information = MaybeUninit::<FILE_BASIC_INFORMATION>::uninit();
    // SAFETY: the object attributes and the output buffer are valid for the call.
    let status = unsafe {
        (DETOUR_NT_QUERY_ATTRIBUTES_FILE.real())(
            &raw mut object_attributes,
            basic_information.as_mut_ptr(),
        )
    };
    let created = matches!(status, STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND);

    // SAFETY: the object attributes are valid for this call.
    unsafe { handle_open(AccessMode::Write, created, &raw mut object_attributes) };
}

static DETOUR_NT_OPEN_FILE: Detour<
    unsafe extern "system" fn(
        file_handle: PHANDLE,
        desired_access: ACCESS_MASK,
        object_attributes: POBJECT_ATTRIBUTES,
        io_status_block: PIO_STATUS_BLOCK,
        share_access: ULONG,
        open_options: ULONG,
    ) -> HFILE,
> =
    // SAFETY: initializing Detour with the real NtOpenFile function pointer and our replacement
    unsafe {
        Detour::new(c"NtOpenFile", ntapi::ntioapi::NtOpenFile, {
            unsafe extern "system" fn new_nt_open_file(
                file_handle: PHANDLE,
                desired_access: ACCESS_MASK,
                object_attributes: POBJECT_ATTRIBUTES,
                io_status_block: PIO_STATUS_BLOCK,
                share_access: ULONG,
                open_options: ULONG,
            ) -> HFILE {
                // SAFETY: intercepting file open to record access before forwarding to real function
                unsafe {
                    handle_open(desired_access, false, object_attributes);
                }

                // SAFETY: calling the original NtOpenFile with all original arguments
                unsafe {
                    (DETOUR_NT_OPEN_FILE.real())(
                        file_handle,
                        desired_access,
                        object_attributes,
                        io_status_block,
                        share_access,
                        open_options,
                    )
                }
            }
            new_nt_open_file
        })
    };

static DETOUR_NT_QUERY_ATTRIBUTES_FILE: Detour<
    unsafe extern "system" fn(
        object_attributes: POBJECT_ATTRIBUTES,
        file_information: PFILE_BASIC_INFORMATION,
    ) -> HFILE,
> =
    // SAFETY: initializing Detour with the real NtQueryAttributesFile function pointer and our replacement
    unsafe {
        Detour::new(c"NtQueryAttributesFile", ntapi::ntioapi::NtQueryAttributesFile, {
            unsafe extern "system" fn new_nt_query_attrs(
                object_attributes: POBJECT_ATTRIBUTES,
                file_information: PFILE_BASIC_INFORMATION,
            ) -> HFILE {
                // SAFETY: intercepting attribute query to record read access
                unsafe { handle_open(AccessMode::Read, false, object_attributes) };
                // SAFETY: calling the original NtQueryAttributesFile with all original arguments
                unsafe {
                    (DETOUR_NT_QUERY_ATTRIBUTES_FILE.real())(object_attributes, file_information)
                }
            }
            new_nt_query_attrs
        })
    };

unsafe fn handle_open(access_mode: impl ToAccessMode, created: bool, path: impl ToAbsolutePath) {
    // SAFETY: accessing the global client which was initialized during DLL_PROCESS_ATTACH
    let client = unsafe { global_client() };
    // SAFETY: resolving path from Windows object attributes or handle for access tracking
    unsafe {
        path.to_absolute_path(|path| {
            let Some(path) = path else {
                return Ok(());
            };
            let path = path.as_slice();
            let path_access = path.iter().rposition(|c| *c == u16::from(b'*')).map_or_else(
                || {
                    // SAFETY: converting access mask to AccessMode via FFI-aware trait
                    access_mode.to_access_mode().with_path(IpcPath::from_wide(path), created)
                },
                |wildcard_pos| {
                    let path_before_wildcard = &path[..wildcard_pos];
                    let slash_pos = path_before_wildcard
                        .iter()
                        .rposition(|c| *c == u16::from(b'\\') || *c == u16::from(b'/'))
                        .unwrap_or(0);
                    PathAccess::ReadDir(IpcPath::from_wide(&path[..slash_pos]))
                },
            );
            client.send(path_access);
            Ok(())
        })
    }
    .unwrap();
}

static DETOUR_NT_FULL_QUERY_ATTRIBUTES_FILE: Detour<
    unsafe extern "system" fn(
        object_attributes: POBJECT_ATTRIBUTES,
        file_information: PFILE_NETWORK_OPEN_INFORMATION,
    ) -> HFILE,
> =
    // SAFETY: initializing Detour with the real NtQueryFullAttributesFile function pointer
    unsafe {
        Detour::new(c"NtQueryFullAttributesFile", NtQueryFullAttributesFile, {
            unsafe extern "system" fn new_fn(
                object_attributes: POBJECT_ATTRIBUTES,
                file_information: PFILE_NETWORK_OPEN_INFORMATION,
            ) -> HFILE {
                // SAFETY: intercepting attribute query to record read access
                unsafe { handle_open(GENERIC_READ, false, object_attributes) };
                // SAFETY: calling the original NtQueryFullAttributesFile
                unsafe {
                    (DETOUR_NT_FULL_QUERY_ATTRIBUTES_FILE.real())(
                        object_attributes,
                        file_information,
                    )
                }
            }
            new_fn
        })
    };

static DETOUR_NT_OPEN_SYMBOLIC_LINK_OBJECT: Detour<
    unsafe extern "system" fn(
        link_handle: PHANDLE,
        desired_access: ACCESS_MASK,
        object_attributes: POBJECT_ATTRIBUTES,
    ) -> HFILE,
> =
    // SAFETY: initializing Detour with the real NtOpenSymbolicLinkObject function pointer
    unsafe {
        Detour::new(c"NtOpenSymbolicLinkObject", ntapi::ntobapi::NtOpenSymbolicLinkObject, {
            unsafe extern "system" fn new_fn(
                link_handle: PHANDLE,
                desired_access: ACCESS_MASK,
                object_attributes: POBJECT_ATTRIBUTES,
            ) -> HFILE {
                // SAFETY: intercepting symlink open to record access
                unsafe { handle_open(desired_access, false, object_attributes) };
                // SAFETY: calling the original NtOpenSymbolicLinkObject
                unsafe {
                    (DETOUR_NT_OPEN_SYMBOLIC_LINK_OBJECT.real())(
                        link_handle,
                        desired_access,
                        object_attributes,
                    )
                }
            }
            new_fn
        })
    };

static DETOUR_NT_QUERY_INFORMATION_BY_NAME: Detour<
    unsafe extern "system" fn(
        object_attributes: POBJECT_ATTRIBUTES,
        io_status_block: PIO_STATUS_BLOCK,
        file_information: PVOID,
        length: ULONG,
        file_information_class: FILE_INFORMATION_CLASS,
    ) -> HFILE,
> =
    // SAFETY: initializing Detour with the real NtQueryInformationByName function pointer
    unsafe {
        Detour::new(c"NtQueryInformationByName", NtQueryInformationByName, {
            unsafe extern "system" fn new_fn(
                object_attributes: POBJECT_ATTRIBUTES,
                io_status_block: PIO_STATUS_BLOCK,
                file_information: PVOID,
                length: ULONG,
                file_information_class: FILE_INFORMATION_CLASS,
            ) -> HFILE {
                // SAFETY: intercepting information query to record read access
                unsafe { handle_open(GENERIC_READ, false, object_attributes) };
                // SAFETY: calling the original NtQueryInformationByName
                unsafe {
                    (DETOUR_NT_QUERY_INFORMATION_BY_NAME.real())(
                        object_attributes,
                        io_status_block,
                        file_information,
                        length,
                        file_information_class,
                    )
                }
            }
            new_fn
        })
    };

static DETOUR_NT_QUERY_DIRECTORY_FILE: Detour<
    unsafe extern "system" fn(
        file_handle: HANDLE,
        event: HANDLE,
        apc_routine: PIO_APC_ROUTINE,
        apc_context: PVOID,
        io_status_block: PIO_STATUS_BLOCK,
        file_information: PVOID,
        length: ULONG,
        file_information_class: FILE_INFORMATION_CLASS,
        return_single_entry: BOOLEAN,
        file_name: PUNICODE_STRING,
        restart_scan: BOOLEAN,
    ) -> NTSTATUS,
> =
    // SAFETY: initializing Detour with the real NtQueryDirectoryFile function pointer
    unsafe {
        Detour::new(c"NtQueryDirectoryFile", NtQueryDirectoryFile, {
            unsafe extern "system" fn new_fn(
                file_handle: HANDLE,
                event: HANDLE,
                apc_routine: PIO_APC_ROUTINE,
                apc_context: PVOID,
                io_status_block: PIO_STATUS_BLOCK,
                file_information: PVOID,
                length: ULONG,
                file_information_class: FILE_INFORMATION_CLASS,
                return_single_entry: BOOLEAN,
                file_name: PUNICODE_STRING,
                restart_scan: BOOLEAN,
            ) -> NTSTATUS {
                // SAFETY: intercepting directory query to record directory read access
                unsafe { handle_open(AccessMode::ReadDir, false, file_handle) };
                // SAFETY: calling the original NtQueryDirectoryFile
                unsafe {
                    (DETOUR_NT_QUERY_DIRECTORY_FILE.real())(
                        file_handle,
                        event,
                        apc_routine,
                        apc_context,
                        io_status_block,
                        file_information,
                        length,
                        file_information_class,
                        return_single_entry,
                        file_name,
                        restart_scan,
                    )
                }
            }
            new_fn
        })
    };

// NtQueryDirectoryFileEx is not in ntapi crate, so we define it here.
// https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntquerydirectoryfileex
type NtQueryDirectoryFileExFn = unsafe extern "system" fn(
    file_handle: HANDLE,
    event: HANDLE,
    apc_routine: PIO_APC_ROUTINE,
    apc_context: PVOID,
    io_status_block: PIO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
    query_flags: ULONG,
    file_name: PUNICODE_STRING,
) -> NTSTATUS;

static DETOUR_NT_QUERY_DIRECTORY_FILE_EX: Detour<NtQueryDirectoryFileExFn> =
    // SAFETY: initializing dynamic Detour for NtQueryDirectoryFileEx (resolved at attach time)
    unsafe {
        Detour::dynamic(c"NtQueryDirectoryFileEx", {
            unsafe extern "system" fn new_fn(
                file_handle: HANDLE,
                event: HANDLE,
                apc_routine: PIO_APC_ROUTINE,
                apc_context: PVOID,
                io_status_block: PIO_STATUS_BLOCK,
                file_information: PVOID,
                length: ULONG,
                file_information_class: FILE_INFORMATION_CLASS,
                query_flags: ULONG,
                file_name: PUNICODE_STRING,
            ) -> NTSTATUS {
                // SAFETY: intercepting directory query to record directory read access
                unsafe { handle_open(AccessMode::ReadDir, false, file_handle) };
                // SAFETY: calling the original NtQueryDirectoryFileEx
                unsafe {
                    (DETOUR_NT_QUERY_DIRECTORY_FILE_EX.real())(
                        file_handle,
                        event,
                        apc_routine,
                        apc_context,
                        io_status_block,
                        file_information,
                        length,
                        file_information_class,
                        query_flags,
                        file_name,
                    )
                }
            }
            new_fn
        })
    };

pub const DETOURS: &[DetourAny] = &[
    DETOUR_NT_CREATE_USER_PROCESS.as_any(),
    DETOUR_NT_CREATE_FILE.as_any(),
    DETOUR_NT_OPEN_FILE.as_any(),
    DETOUR_NT_SET_INFORMATION_FILE.as_any(),
    DETOUR_NT_QUERY_ATTRIBUTES_FILE.as_any(),
    DETOUR_NT_FULL_QUERY_ATTRIBUTES_FILE.as_any(),
    DETOUR_NT_OPEN_SYMBOLIC_LINK_OBJECT.as_any(),
    DETOUR_NT_QUERY_INFORMATION_BY_NAME.as_any(),
    DETOUR_NT_QUERY_DIRECTORY_FILE.as_any(),
    DETOUR_NT_QUERY_DIRECTORY_FILE_EX.as_any(),
];
