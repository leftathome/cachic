//! cachic binary entry point.
//!
//! The proxy is not implemented yet; see `.agent/tasks/TASK-INDEX.md`. The M0 spike is a
//! separate binary (`spike`).

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => println!("cachic {}", cachic::VERSION),
        Some(other) => {
            eprintln!(
                "cachic {}: unrecognised argument {other:?}",
                cachic::VERSION
            );
            eprintln!("the proxy is not implemented yet; see .agent/tasks/TASK-INDEX.md");
            std::process::exit(2);
        }
        None => {
            eprintln!("cachic {}: not implemented yet", cachic::VERSION);
            eprintln!("see .agent/tasks/TASK-INDEX.md for the milestone plan");
            std::process::exit(2);
        }
    }
}
