//! Release tooling: print the catalog of a marketplace tree.
use std::path::Path;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        eprintln!("usage: marketplace <marketplace-directory>");
        std::process::exit(2);
    }
    match appa_package::Marketplace::render(Path::new(&args[0])) {
        Ok(catalog) => print!("{catalog}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
