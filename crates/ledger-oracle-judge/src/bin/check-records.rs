//! Checks the records of one layer in a directory named by their reference
//! pin, printing their counts and each reason they cannot be judged.

use std::{path::Path, process::ExitCode};

use ledger_oracle_judge::comparison::{Coverage, Layer, Records};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(dir), Some(layer)) = (args.first(), args.get(1).and_then(|l| Layer::from_name(l)))
    else {
        eprintln!("usage: check-records DIR LAYER [KIND or CLASS...]");
        return ExitCode::from(2);
    };
    let records = match Records::read_dir(Path::new(dir), layer) {
        Ok(records) => records,
        Err(e) => {
            println!("{e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "records: {} rows of {} at {} by {}",
        records.rows().len(),
        records.layer(),
        records.pin(),
        records.generator()
    );
    for (class, n) in records.rows_per_class() {
        println!("class {class:?}: {n} rows");
    }
    let coverage = Coverage::new(records.layer(), records.targets(), records.rows());
    for (kind, (targets, made)) in coverage.kinds() {
        println!("kind {kind}: made {made} of {targets} targets");
    }
    let failures = records.check(layer, &args[2..]);
    for failure in &failures {
        println!("{failure}");
    }
    if failures.is_empty() {
        println!("records pass");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
