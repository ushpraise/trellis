/// Input sanitization for user-facing CLI parameters.
///
/// Prevents injection attacks and encoding issues by rejecting or normalizing
/// user-supplied arguments before they reach the RPC layer.

/// Sanitize a hex-encoded agreement ID.
///
/// Rejects null bytes, control characters (0x00-0x1F), and non-ASCII characters.
/// Hex IDs should already be validated for format, this ensures no embedded
/// null bytes or control chars slip through.
pub fn sanitize_hex_id(id: &str) -> Result<String, String> {
    if id.contains('\0') {
        return Err("agreement_id contains null bytes".to_string());
    }
    if id.chars().any(|c| c.is_control()) {
        return Err("agreement_id contains control characters".to_string());
    }
    if !id.is_ascii() {
        return Err("agreement_id must be ASCII-only".to_string());
    }
    Ok(id.to_string())
}

/// Sanitize a proof URI.
///
/// Rejects null bytes, control characters (0x00-0x1F), and normalizes
/// Unicode to NFC to prevent homoglyph attacks.
pub fn sanitize_proof_uri(uri: &str) -> Result<String, String> {
    if uri.contains('\0') {
        return Err("proof_uri contains null bytes".to_string());
    }
    if uri.chars().any(|c| c.is_control()) {
        return Err("proof_uri contains control characters".to_string());
    }

    // Normalize to NFC to prevent homoglyph attacks.
    use unicode_normalization::UnicodeNormalization;
    let normalized: String = uri.nfc().collect();
    Ok(normalized)
}
