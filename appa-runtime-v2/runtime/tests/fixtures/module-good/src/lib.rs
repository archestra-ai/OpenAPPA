use std::sync::atomic::{AtomicBool, Ordering};

use appa_builtin::{BuiltinError, BuiltinInput, export_builtin_authority};

static IN_CALL: AtomicBool = AtomicBool::new(false);

fn fixture(input: &BuiltinInput) -> Result<serde_json::Value, BuiltinError> {
    match input.payload.get("mode").and_then(|mode| mode.as_str()) {
        Some("error") => Err(BuiltinError::new("the fixture declined")),
        Some("panic") => panic!("a deliberate fixture panic"),
        Some("big") => Ok(serde_json::json!({ "filler": "x".repeat(1024 * 1024) })),
        Some("gate") => {
            let overlapped = IN_CALL.swap(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(80));
            IN_CALL.store(false, Ordering::SeqCst);
            Ok(serde_json::json!({ "ruling": "approve", "overlapped": overlapped }))
        }
        _ => Ok(serde_json::json!({ "ruling": "approve", "component": input.component })),
    }
}

export_builtin_authority!("fixture-auth", fixture);
