#![cfg(feature = "postgres")]

use appa_eventlog::encryption::StorageCipher;

#[test]
fn existing_plaintext_tool_outputs_are_not_encrypted_envelopes() {
    // Before storage encryption existed, approved_output was written verbatim.
    // Upgrading must not interpret ordinary tool content as storage metadata.
    let encrypted = StorageCipher::encrypted(&[7; 32], "other-conversation".into())
        .unwrap()
        .seal_text("result", "private output")
        .unwrap();
    for output in ["ordinary result", "appa-storage-v1:user content", &encrypted] {
        assert_eq!(StorageCipher::Plain.seal_text("result", output).unwrap(), output);
        assert_eq!(
            StorageCipher::Plain.open_text("result", output.to_owned()).unwrap(),
            output
        );
    }
}
