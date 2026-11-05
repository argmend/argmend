use argmend::{RepairOutcome, repair_json};

fn main() {
    match repair_json(r#"{file: "a.rs"}"#) {
        RepairOutcome::Repaired { value, repairs } => {
            println!("{value}");
            println!("{} rewrites", repairs.len());
        }
        RepairOutcome::Valid(value) => println!("{value}"),
        RepairOutcome::Unfixable(reason) | RepairOutcome::Ambiguous(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}
