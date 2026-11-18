//! Any bytes through the EDIFACT reader: it may refuse, and must return.
#![no_main]

use erpcraft_edi::edifact;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = edifact::read(data);
});
