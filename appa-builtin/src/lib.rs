//! appa-builtin — the author surface for builtin modules.

use serde::Deserialize;

/// The ABI version this crate generates. The loader requires exact
/// equality before resolving any other symbol, so a module built against
/// an earlier request shape is refused when the deployment opens, never
/// left to fail every consult.
pub const ABI_VERSION: u32 = 2;

pub const KIND_AUTHORITY: u32 = 1;
pub const KIND_SANITIZER: u32 = 2;

pub const STATUS_OK: u32 = 0;
pub const STATUS_INVALID_INPUT: u32 = 1;
pub const STATUS_BUILTIN_ERROR: u32 = 2;
pub const STATUS_PANICKED: u32 = 3;
pub const STATUS_OUTPUT_TOO_LARGE: u32 = 4;

/// What `appa_builtin_descriptor_v1` returns: the implementation name as
/// module-owned static UTF-8 bytes, and the kind. The host copies the
/// name out immediately and validates it; the filename means nothing.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DescriptorV1 {
    pub name_ptr: *const u8,
    pub name_len: usize,
    pub kind: u32,
}

/// One consult, as the author function sees it: which registered
/// component is being consulted, what the policy declared about it, and
/// the artifact it judges. The same request the HTTP wire carries
/// (`{"version":1,"kind","name","declaration","artifact"}`), with the
/// transport framing already stripped.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinInput {
    /// The registered component under consult (e.g. the authority name
    /// from the deployment's externals configuration) — one module may
    /// back several components and distinguish them here.
    pub component: String,
    /// The component's declaration, exactly as an HTTP implementation
    /// would receive it.
    pub declaration: serde_json::Value,
    /// The artifact under judgment, exactly as an HTTP implementation
    /// would receive it.
    pub artifact: serde_json::Value,
}

/// The author function declined to answer. The runtime treats it as a
/// consult with no answer — never a denial. The message stays
/// module-side; it never crosses the boundary.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct BuiltinError {
    message: String,
}

impl BuiltinError {
    pub fn new(message: impl Into<String>) -> BuiltinError {
        BuiltinError {
            message: message.into(),
        }
    }
}

pub type BuiltinFn = fn(&BuiltinInput) -> Result<serde_json::Value, BuiltinError>;

#[derive(Deserialize)]
struct RequestEnvelope {
    version: u32,
    kind: String,
    name: String,
    declaration: serde_json::Value,
    artifact: serde_json::Value,
}

/// Support functions the export macros call. Not part of the authoring
/// surface; the macro is the contract.
#[doc(hidden)]
pub mod __support {
    use super::*;

    pub fn descriptor(name: &'static str, kind: u32) -> DescriptorV1 {
        DescriptorV1 {
            name_ptr: name.as_ptr(),
            name_len: name.len(),
            kind,
        }
    }

    /// The answer entry point's whole body. Catches every unwinding
    /// panic so none reaches the `extern "C"` boundary (an escaped
    /// panic there aborts the process). The catch covers unwinding
    /// panics only: a module built with `panic = "abort"`, a double
    /// panic, or a stack overflow still terminates the process, so a
    /// module crate MUST keep the default `panic = "unwind"` profile.
    pub unsafe fn answer(
        func: BuiltinFn,
        expected_kind: u32,
        input_ptr: *const u8,
        input_len: usize,
        output_ptr: *mut u8,
        output_capacity: usize,
        output_len: *mut usize,
    ) -> u32 {
        if output_len.is_null() {
            return STATUS_INVALID_INPUT;
        }
        unsafe { *output_len = 0 };
        if input_ptr.is_null() {
            return STATUS_INVALID_INPUT;
        }
        // Refuse overlapping buffers: aliased slices would be undefined
        // behavior, and the host never passes them. The end addresses
        // use checked adds — an overflowing range is as invalid as an
        // overlapping one, and a panic here would unwind through the
        // `extern "C"` boundary, before the catch below exists.
        let (Some(input_end), Some(output_end), Some(length_end)) = (
            (input_ptr as usize).checked_add(input_len),
            (output_ptr as usize).checked_add(output_capacity),
            (output_len as usize).checked_add(size_of::<usize>()),
        ) else {
            return STATUS_INVALID_INPUT;
        };
        let input_range = input_ptr as usize..input_end;
        let output_range = output_ptr as usize..output_end;
        let length_range = output_len as usize..length_end;
        if ranges_overlap(&input_range, &output_range)
            || ranges_overlap(&input_range, &length_range)
            || ranges_overlap(&output_range, &length_range)
        {
            return STATUS_INVALID_INPUT;
        }
        let input = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
        let output: &mut [u8] = if output_capacity == 0 {
            &mut []
        } else if output_ptr.is_null() {
            return STATUS_INVALID_INPUT;
        } else {
            unsafe { std::slice::from_raw_parts_mut(output_ptr, output_capacity) }
        };
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(func, expected_kind, input, output)));
        match outcome {
            Ok((status, written)) => {
                unsafe { *output_len = written };
                status
            }
            Err(payload) => {
                // Dropping a panic payload can itself panic; leak it.
                std::mem::forget(payload);
                STATUS_PANICKED
            }
        }
    }

    fn ranges_overlap(a: &std::ops::Range<usize>, b: &std::ops::Range<usize>) -> bool {
        a.start < b.end && b.start < a.end
    }

    fn run(func: BuiltinFn, expected_kind: u32, input: &[u8], output: &mut [u8]) -> (u32, usize) {
        let envelope: RequestEnvelope = match serde_json::from_slice(input) {
            Ok(envelope) => envelope,
            Err(_) => return (STATUS_INVALID_INPUT, 0),
        };
        if envelope.version != 1 {
            return (STATUS_INVALID_INPUT, 0);
        }
        let expected_wire = match expected_kind {
            KIND_AUTHORITY => "authority",
            KIND_SANITIZER => "sanitizer",
            _ => return (STATUS_INVALID_INPUT, 0),
        };
        if envelope.kind != expected_wire {
            return (STATUS_INVALID_INPUT, 0);
        }
        let builtin_input = BuiltinInput {
            component: envelope.name,
            declaration: envelope.declaration,
            artifact: envelope.artifact,
        };
        let answer = match func(&builtin_input) {
            Ok(answer) => answer,
            Err(_) => return (STATUS_BUILTIN_ERROR, 0),
        };
        let mut cursor = std::io::Cursor::new(output);
        if serde_json::to_writer(&mut cursor, &answer).is_err() {
            return (STATUS_OUTPUT_TOO_LARGE, 0);
        }
        let written = usize::try_from(cursor.position()).expect("a buffer position fits usize");
        (STATUS_OK, written)
    }
}

#[macro_export]
macro_rules! export_builtin_authority {
    ($name:literal, $func:path) => {
        $crate::__export_builtin!($name, $func, $crate::KIND_AUTHORITY);
    };
}

#[macro_export]
macro_rules! export_builtin_sanitizer {
    ($name:literal, $func:path) => {
        $crate::__export_builtin!($name, $func, $crate::KIND_SANITIZER);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __export_builtin {
    ($name:literal, $func:path, $kind:expr) => {
        #[unsafe(export_name = "appa_builtin_abi_version")]
        pub extern "C" fn __appa_builtin_abi_version() -> u32 {
            $crate::ABI_VERSION
        }

        #[unsafe(export_name = "appa_builtin_descriptor_v1")]
        pub extern "C" fn __appa_builtin_descriptor_v1() -> $crate::DescriptorV1 {
            $crate::__support::descriptor($name, $kind)
        }

        #[unsafe(export_name = "appa_builtin_answer_v1")]
        pub unsafe extern "C" fn __appa_builtin_answer_v1(
            input_ptr: *const u8,
            input_len: usize,
            output_ptr: *mut u8,
            output_capacity: usize,
            output_len: *mut usize,
        ) -> u32 {
            unsafe {
                $crate::__support::answer(
                    $func,
                    $kind,
                    input_ptr,
                    input_len,
                    output_ptr,
                    output_capacity,
                    output_len,
                )
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo(input: &BuiltinInput) -> Result<serde_json::Value, BuiltinError> {
        match input.artifact.get("mode").and_then(|mode| mode.as_str()) {
            Some("error") => Err(BuiltinError::new("declined")),
            Some("panic") => panic!("a deliberate test panic"),
            _ => Ok(serde_json::json!({
                "component": input.component,
                "declaration": input.declaration,
                "artifact": input.artifact,
            })),
        }
    }

    export_builtin_sanitizer!("test-echo", echo);

    fn call(request: &[u8], capacity: usize) -> (u32, Vec<u8>) {
        let mut output = vec![0u8; capacity];
        let mut written: usize = usize::MAX;
        let status = unsafe {
            __appa_builtin_answer_v1(
                request.as_ptr(),
                request.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut written,
            )
        };
        assert!(written <= capacity, "the wrapper never reports beyond capacity");
        output.truncate(written);
        (status, output)
    }

    fn request(artifact: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "kind": "sanitizer",
            "name": "pii",
            "declaration": {"on": "tool_output"},
            "artifact": artifact,
        }))
        .expect("the request envelope serializes")
    }

    #[test]
    fn the_exported_version_and_descriptor_identify_the_module() {
        assert_eq!(__appa_builtin_abi_version(), ABI_VERSION);
        let descriptor = __appa_builtin_descriptor_v1();
        let name = unsafe { std::slice::from_raw_parts(descriptor.name_ptr, descriptor.name_len) };
        assert_eq!(name, b"test-echo");
        assert_eq!(descriptor.kind, KIND_SANITIZER);
    }

    #[test]
    fn a_successful_answer_carries_the_component_declaration_and_artifact() {
        let (status, body) = call(&request(serde_json::json!({"value": "x"})), 4096);
        assert_eq!(status, STATUS_OK);
        let answer: serde_json::Value = serde_json::from_slice(&body).expect("the answer is JSON");
        assert_eq!(answer["component"], "pii");
        assert_eq!(answer["declaration"]["on"], "tool_output");
        assert_eq!(answer["artifact"]["value"], "x");
    }

    #[test]
    fn every_failure_reports_its_status_and_no_bytes() {
        let (status, body) = call(&request(serde_json::json!({"mode": "error"})), 4096);
        assert_eq!((status, body.len()), (STATUS_BUILTIN_ERROR, 0));

        let (status, body) = call(&request(serde_json::json!({"mode": "panic"})), 4096);
        assert_eq!((status, body.len()), (STATUS_PANICKED, 0));

        let (status, body) = call(&request(serde_json::json!({"value": "y".repeat(100)})), 16);
        assert_eq!((status, body.len()), (STATUS_OUTPUT_TOO_LARGE, 0));

        let (status, body) = call(b"not json", 4096);
        assert_eq!((status, body.len()), (STATUS_INVALID_INPUT, 0));

        let envelope = serde_json::to_vec(&serde_json::json!({
            "version": 2, "kind": "sanitizer", "name": "pii", "declaration": {}, "artifact": {},
        }))
        .expect("the envelope serializes");
        let (status, body) = call(&envelope, 4096);
        assert_eq!((status, body.len()), (STATUS_INVALID_INPUT, 0));

        let envelope = serde_json::to_vec(&serde_json::json!({"version": 1})).expect("the envelope serializes");
        let (status, body) = call(&envelope, 4096);
        assert_eq!((status, body.len()), (STATUS_INVALID_INPUT, 0));

        let envelope = serde_json::to_vec(&serde_json::json!({
            "version": 1, "kind": "authority", "name": "pii", "declaration": {}, "artifact": {},
        }))
        .expect("the envelope serializes");
        let (status, body) = call(&envelope, 4096);
        assert_eq!((status, body.len()), (STATUS_INVALID_INPUT, 0));
    }

    #[test]
    fn a_panicking_panic_payload_is_leaked_not_repropagated() {
        struct DropBomb;
        impl Drop for DropBomb {
            fn drop(&mut self) {
                panic!("the payload's destructor panics");
            }
        }
        fn detonate(_input: &BuiltinInput) -> Result<serde_json::Value, BuiltinError> {
            std::panic::panic_any(DropBomb);
        }
        let request = request(serde_json::json!({}));
        let mut output = vec![0u8; 64];
        let mut written: usize = 0;
        let status = unsafe {
            __support::answer(
                detonate,
                KIND_SANITIZER,
                request.as_ptr(),
                request.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut written,
            )
        };
        assert_eq!((status, written), (STATUS_PANICKED, 0));
    }

    #[test]
    fn null_pointers_are_refused_without_undefined_behavior() {
        let request = request(serde_json::json!({}));
        let mut output = vec![0u8; 64];
        let mut written: usize = 0;

        let status =
            unsafe { __appa_builtin_answer_v1(std::ptr::null(), 8, output.as_mut_ptr(), output.len(), &mut written) };
        assert_eq!(status, STATUS_INVALID_INPUT);

        let status = unsafe {
            __appa_builtin_answer_v1(request.as_ptr(), request.len(), std::ptr::null_mut(), 64, &mut written)
        };
        assert_eq!(status, STATUS_INVALID_INPUT);

        let status =
            unsafe { __appa_builtin_answer_v1(request.as_ptr(), request.len(), std::ptr::null_mut(), 0, &mut written) };
        assert_eq!((status, written), (STATUS_OUTPUT_TOO_LARGE, 0));
    }

    #[test]
    fn overlapping_buffers_are_refused() {
        let mut arena = request(serde_json::json!({}));
        arena.resize(arena.len() + 64, 0);
        let len = arena.len();
        let mut written: usize = 0;
        let status = unsafe { __appa_builtin_answer_v1(arena.as_ptr(), len, arena.as_mut_ptr(), len, &mut written) };
        assert_eq!(status, STATUS_INVALID_INPUT);
    }

    #[test]
    fn null_output_length_is_refused_without_a_write() {
        let request = request(serde_json::json!({}));
        let mut output = vec![0u8; 64];
        let status = unsafe {
            __appa_builtin_answer_v1(
                request.as_ptr(),
                request.len(),
                output.as_mut_ptr(),
                output.len(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, STATUS_INVALID_INPUT);
    }

    #[test]
    fn a_zero_capacity_buffer_is_only_too_small_never_unsound() {
        let (status, body) = call(&request(serde_json::json!({})), 0);
        assert_eq!((status, body.len()), (STATUS_OUTPUT_TOO_LARGE, 0));
    }

    #[test]
    fn the_authority_kind_descriptor_names_the_other_kind() {
        let descriptor = __support::descriptor("auto", KIND_AUTHORITY);
        assert_eq!(descriptor.kind, KIND_AUTHORITY);
        let name = unsafe { std::slice::from_raw_parts(descriptor.name_ptr, descriptor.name_len) };
        assert_eq!(name, b"auto");
    }
}
