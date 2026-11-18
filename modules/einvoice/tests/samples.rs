//! Reads every `.xml` file in the directory `EINVOICE_SAMPLES` names, such
//! as the business cases of the KoSIT XRechnung test suite
//! (github.com/itplr-kosit/xrechnung-testsuite, src/test/business-cases),
//! and checks it with the rules this crate checks. Without the variable it
//! does nothing: the samples are not part of this repository.

#[test]
fn official_samples_read_and_hold_the_rules_checked_here() {
    let Ok(dir) = std::env::var("EINVOICE_SAMPLES") else {
        return;
    };
    let mut read = 0;
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("EINVOICE_SAMPLES is a directory") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "xml") {
            continue;
        }
        let content = std::fs::read_to_string(&path).unwrap();
        match erpcraft_einvoice::read_xml(&content) {
            Ok(invoice) => {
                read += 1;
                let fatal: Vec<String> = erpcraft_einvoice::model::check(&invoice)
                    .into_iter()
                    .filter(|f| f.fatal)
                    .map(|f| format!("{} {}", f.rule, f.message))
                    .collect();
                if !fatal.is_empty() {
                    failures.push(format!("{}: {fatal:?}", path.display()));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    assert!(read > 0, "no XML files in {dir}");
    assert!(failures.is_empty(), "{read} read; {}", failures.join("\n"));
}
