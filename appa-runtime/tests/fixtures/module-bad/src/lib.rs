#[repr(C)]
pub struct RawDescriptor {
    pub name_ptr: *const u8,
    pub name_len: usize,
    pub kind: u32,
}

#[cfg(feature = "bad-abi")]
mod variant {
    use super::RawDescriptor;

    #[unsafe(export_name = "appa_builtin_abi_version")]
    pub extern "C" fn version() -> u32 {
        99
    }

    #[unsafe(export_name = "appa_builtin_descriptor_v1")]
    pub extern "C" fn descriptor() -> RawDescriptor {
        static NAME: &[u8] = b"future";
        RawDescriptor {
            name_ptr: NAME.as_ptr(),
            name_len: NAME.len(),
            kind: 1,
        }
    }

    #[unsafe(export_name = "appa_builtin_answer_v1")]
    pub extern "C" fn answer(_: *const u8, _: usize, _: *mut u8, _: usize, _: *mut usize) -> u32 {
        0
    }
}

#[cfg(feature = "missing-symbol")]
mod variant {
    #[unsafe(export_name = "appa_builtin_abi_version")]
    pub extern "C" fn version() -> u32 {
        2
    }
}

#[cfg(feature = "bad-name")]
mod variant {
    use super::RawDescriptor;

    #[unsafe(export_name = "appa_builtin_abi_version")]
    pub extern "C" fn version() -> u32 {
        2
    }

    #[unsafe(export_name = "appa_builtin_descriptor_v1")]
    pub extern "C" fn descriptor() -> RawDescriptor {
        static NAME: &[u8] = b"Not-Kebab";
        RawDescriptor {
            name_ptr: NAME.as_ptr(),
            name_len: NAME.len(),
            kind: 2,
        }
    }

    #[unsafe(export_name = "appa_builtin_answer_v1")]
    pub extern "C" fn answer(_: *const u8, _: usize, _: *mut u8, _: usize, _: *mut usize) -> u32 {
        0
    }
}

#[cfg(feature = "dishonest-length")]
mod variant {
    use super::RawDescriptor;

    #[unsafe(export_name = "appa_builtin_abi_version")]
    pub extern "C" fn version() -> u32 {
        2
    }

    #[unsafe(export_name = "appa_builtin_descriptor_v1")]
    pub extern "C" fn descriptor() -> RawDescriptor {
        static NAME: &[u8] = b"liar";
        RawDescriptor {
            name_ptr: NAME.as_ptr(),
            name_len: NAME.len(),
            kind: 1,
        }
    }

    #[unsafe(export_name = "appa_builtin_answer_v1")]
    pub unsafe extern "C" fn answer(
        _: *const u8,
        _: usize,
        _: *mut u8,
        capacity: usize,
        output_len: *mut usize,
    ) -> u32 {
        unsafe { *output_len = capacity + 1 };
        0 // STATUS_OK, dishonestly.
    }
}

#[cfg(feature = "same-name-sanitizer")]
mod variant {
    use super::RawDescriptor;

    #[unsafe(export_name = "appa_builtin_abi_version")]
    pub extern "C" fn version() -> u32 {
        2
    }

    #[unsafe(export_name = "appa_builtin_descriptor_v1")]
    pub extern "C" fn descriptor() -> RawDescriptor {
        static NAME: &[u8] = b"fixture-auth";
        RawDescriptor {
            name_ptr: NAME.as_ptr(),
            name_len: NAME.len(),
            kind: 2,
        }
    }

    #[unsafe(export_name = "appa_builtin_answer_v1")]
    pub extern "C" fn answer(_: *const u8, _: usize, _: *mut u8, _: usize, _: *mut usize) -> u32 {
        0
    }
}

#[cfg(feature = "claims-approve")]
mod variant {
    use super::RawDescriptor;

    #[unsafe(export_name = "appa_builtin_abi_version")]
    pub extern "C" fn version() -> u32 {
        2
    }

    #[unsafe(export_name = "appa_builtin_descriptor_v1")]
    pub extern "C" fn descriptor() -> RawDescriptor {
        static NAME: &[u8] = b"approve";
        RawDescriptor {
            name_ptr: NAME.as_ptr(),
            name_len: NAME.len(),
            kind: 1,
        }
    }

    #[unsafe(export_name = "appa_builtin_answer_v1")]
    pub extern "C" fn answer(_: *const u8, _: usize, _: *mut u8, _: usize, _: *mut usize) -> u32 {
        0
    }
}
