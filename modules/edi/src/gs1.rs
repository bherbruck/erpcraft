//! GS1 check digits and the serial shipping container code (SSCC).
//!
//! Every carton or pallet on an advance ship notice is named by its SSCC,
//! eighteen digits printed on its GS1-128 label after the application
//! identifier `(00)`: an extension digit the shipper chooses, the
//! shipper's GS1 company prefix, a serial reference that fills the rest,
//! and a check digit. The receiving dock scans the label and looks the
//! number up in the notice, so a wrong digit is a carton nobody ordered.

/// The check digit of a GS1 key (GTIN-8, -12, -13 and -14, GLN, SSCC):
/// from the right, the digits are weighted 3, 1, 3, 1 and so on, and the
/// check digit brings the sum to a multiple of ten. `digits` is the key
/// without its check digit.
pub fn check_digit(digits: &str) -> Option<u8> {
    if digits.is_empty() || digits.len() > 17 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let sum: u32 = digits
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    Some(((10 - sum % 10) % 10) as u8)
}

/// Whether a GS1 key ends in its right check digit.
pub fn is_valid(key: &str) -> bool {
    key.len() >= 2
        && key.is_ascii()
        && check_digit(&key[..key.len() - 1])
            .is_some_and(|d| key.as_bytes()[key.len() - 1] == b'0' + d)
}

/// The SSCC of one logistic unit. Refused, with nothing returned, when
/// the extension is not one digit, the prefix is not 7 to 10 digits, or
/// the serial does not fit the digits the prefix leaves: a serial that is
/// cut to fit would name another carton.
pub fn sscc(extension: u8, company_prefix: &str, serial: u64) -> Option<String> {
    let prefix_ok = (7..=10).contains(&company_prefix.len())
        && company_prefix.bytes().all(|b| b.is_ascii_digit());
    if extension > 9 || !prefix_ok {
        return None;
    }
    let width = 16 - company_prefix.len();
    let serial = serial.to_string();
    if serial.len() > width {
        return None;
    }
    let body = format!("{extension}{company_prefix}{serial:0>width$}");
    let check = check_digit(&body)?;
    Some(format!("{body}{check}"))
}
