//! The acknowledgements phase 1 adds to the spike: the 999 as a second
//! form of the 997, the TA1 for an interchange, and the sentence per
//! segment. Each is written, read back with the reader a partner's would
//! meet, and survives every change of a byte.

use chrono::NaiveDate;
use erpcraft_edi::explain::explain;
use erpcraft_edi::inbound::{SetRead, read_file};
use erpcraft_edi::x12::ack::{AckForm, FunctionalAck, Status, describe};
use erpcraft_edi::x12::ta1::{NOTE_DUPLICATE, NOTE_GROUP_COUNT, Ta1, Ta1Status};
use erpcraft_edi::x12::{self, Envelope, po850};

const PO850: &str = include_str!("fixtures/po850.x12");

fn at() -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 18)
        .unwrap()
        .and_hms_opt(9, 31, 0)
        .unwrap()
}

#[test]
fn a_999_says_what_the_997_says_with_its_own_tags() {
    let broken = PO850.replace("DTM*038*20261205", "DTM*038*20261305");
    let interchange = &x12::read(broken.as_bytes()).unwrap()[0];
    let group = &interchange.groups[0];
    let ack = FunctionalAck::of(group, |set| po850::read(set).1);
    let mut envelope = Envelope::reply_to(interchange, group, "FA", at(), 7, 7);
    envelope.version = "00501".into();
    envelope.group_version = "005010".into();
    let body = ack.body_as(
        &envelope.delimiters,
        &AckForm::Ack999 {
            version: "005010".into(),
            conventions: vec![],
        },
    );
    let written = x12::write(&envelope, "999", &[body]).unwrap();
    assert!(
        written.contains(
            "ST*999*0001~AK1*PO*417*005010~AK2*850*0001*005010~IK3*DTM*9**8~IK4*2**8*20261305~\
             IK5*R*5~AK9*R*1*1*0~"
        ),
        "{written}"
    );
    let back = &x12::read(written.as_bytes()).unwrap()[0];
    let read = FunctionalAck::read(&back.groups[0].sets[0]).unwrap();
    assert_eq!(read, ack);
    assert_eq!(read.sets[0].status, Status::Rejected);

    // A 999 with a context segment reads the same.
    let with_context = written.replace("IK3*DTM*9**8~", "IK3*DTM*9**8~CTX*SITUATIONAL TRIGGER~");
    let with_context = with_context.replace("SE*7*0001", "SE*8*0001");
    let back = &x12::read(with_context.as_bytes()).unwrap()[0];
    assert_eq!(FunctionalAck::read(&back.groups[0].sets[0]).unwrap(), ack);

    // The words a person reads.
    let words = describe(&ack.sets[0]).join(" ");
    assert!(
        words.contains("Segment DTM at position 9 has errors."),
        "{words}"
    );
    assert!(words.contains("Element 2 of DTM is not a date"), "{words}");
}

#[test]
fn a_ta1_answers_an_interchange_and_reads_back() {
    let interchange = &x12::read(PO850.as_bytes()).unwrap()[0];
    let accepted = Ta1::of(interchange);
    assert_eq!(accepted.status, Ta1Status::Accepted);
    assert_eq!(accepted.note, "000");

    let wrong = PO850.replace("IEA*1*000000417", "IEA*2*000000417");
    let counted = Ta1::of(&x12::read(wrong.as_bytes()).unwrap()[0]);
    assert_eq!(
        (counted.status, counted.note.as_str()),
        (Ta1Status::AcceptedWithErrors, NOTE_GROUP_COUNT)
    );

    let duplicate = Ta1::answering(&interchange.isa, Ta1Status::Rejected, NOTE_DUPLICATE);
    let envelope = Envelope::reply_to(interchange, &interchange.groups[0], "FA", at(), 9, 0);
    let written = x12::write_ta1s(&envelope, &[duplicate.segment()]).unwrap();
    assert!(
        written.ends_with("TA1*000000417*261118*0930*R*025~IEA*0*000000009~"),
        "{written}"
    );
    let back = &x12::read(written.as_bytes()).unwrap()[0];
    assert!(back.errors.is_empty() && back.groups.is_empty());
    assert_eq!(Ta1::read(&back.ta1[0]).unwrap(), duplicate);
}

#[test]
fn every_segment_the_readers_know_has_a_sentence() {
    let interchange = &x12::read(PO850.as_bytes()).unwrap()[0];
    let set = &interchange.groups[0].sets[0];
    let said: Vec<String> = set.segments.iter().map(|s| explain(s, "850")).collect();
    assert_eq!(
        said[1],
        "Order 4500018842 dated 2026-11-18, purpose 00, type SA"
    );
    let po1 = said.iter().find(|s| s.starts_with("Line 1")).unwrap();
    assert_eq!(
        po1,
        "Line 1: 48 EA at 12.5; UP 040000100010, VN CH-1001, IN 88213001"
    );
    assert!(said.contains(&"Ship no later than: 2026-12-05".to_owned()));
    // A tag nobody here reads is its tag.
    let unknown = erpcraft_edi::Segment::new("ZZZ", &["1"]);
    assert_eq!(explain(&unknown, "850"), "ZZZ");
}

#[test]
fn no_change_to_an_acknowledgement_makes_reading_it_panic() {
    let interchange = &x12::read(PO850.as_bytes()).unwrap()[0];
    let group = &interchange.groups[0];
    let ack = FunctionalAck::of(group, |set| po850::read(set).1);
    let envelope = Envelope::reply_to(interchange, group, "FA", at(), 52, 52);
    let nine = x12::write(
        &envelope,
        "999",
        &[ack.body_as(
            &envelope.delimiters,
            &AckForm::Ack999 {
                version: "005010".into(),
                conventions: vec![],
            },
        )],
    )
    .unwrap();
    let seven = x12::write(&envelope, "997", &[ack.body(&envelope.delimiters)]).unwrap();
    let ta1 = x12::write_ta1s(&envelope, &[Ta1::of(interchange).segment()]).unwrap();
    for fixture in [nine, seven, ta1] {
        let bytes = fixture.as_bytes();
        for at in 0..bytes.len() {
            for with in [b'*', b'~', b'>', b'9', b'-', b'A', b'R', 0, 0xFF] {
                let mut changed = bytes.to_vec();
                changed[at] = with;
                if let Ok(files) = read_file(&changed) {
                    for file in &files {
                        for set in file.sets.iter().flatten() {
                            if let SetRead::Ack(Ok(ack)) = set {
                                for set in &ack.sets {
                                    let _ = describe(set);
                                }
                            }
                        }
                        for segment in &file.interchange.ta1 {
                            let _ = Ta1::read(segment);
                        }
                    }
                }
                if let Ok(interchanges) = x12::read(&changed) {
                    for segment in interchanges
                        .iter()
                        .flat_map(|i| &i.groups)
                        .flat_map(|g| &g.sets)
                        .flat_map(|s| &s.segments)
                    {
                        let _ = explain(segment, "997");
                    }
                }
            }
        }
    }
}
