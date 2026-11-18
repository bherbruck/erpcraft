//! Any bytes through the X12 reader, the 850 reader, the acknowledgement
//! built for them and its writer: reading may refuse, and must return.
#![no_main]

use erpcraft_edi::inbound::{SetRead, read_file};
use erpcraft_edi::x12::ack::FunctionalAck;
use erpcraft_edi::x12::{self, po850};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(files) = read_file(data) else {
        return;
    };
    for file in &files {
        for group in &file.interchange.groups {
            let ack = FunctionalAck::of(group, |set| po850::read(set).1);
            let _ = ack.body(&file.interchange.isa.delimiters);
        }
        for set in file.sets.iter().flatten() {
            if let SetRead::Order(order, _) = set {
                let _ = touch(order);
            }
        }
    }
    let _ = x12::read(data);
});

/// Touch every field of an order, as a proposal would.
fn touch(order: &po850::PurchaseOrder) -> usize {
    order.lines.iter().map(|l| l.product_ids.len()).sum::<usize>() + order.notes.len()
}
