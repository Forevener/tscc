use std::env;
use std::fs;
use std::path::Path;
use std::process;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "compile" => run_compile(&args[2..]),
        "--help" | "-h" | "help" => {
            print_usage();
            process::exit(0);
        }
        "--version" | "-V" => {
            println!("tscc {VERSION}");
            process::exit(0);
        }
        other => {
            eprintln!("error: unknown command '{other}'");
            eprintln!();
            print_usage();
            process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("tscc {VERSION} — TypeScript to WebAssembly AOT compiler");
    eprintln!();
    eprintln!("Usage: tscc compile <input.ts> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -o <file>            Output .wasm file (default: <input>.wasm)");
    eprintln!("  --host-module <name> WASM import module name (default: \"host\")");
    eprintln!("  --memory-pages <n>   Initial linear memory pages (default: 1, 64KB each)");
    eprintln!("  --debug, -g          Emit DWARF debug info and name section");
    eprintln!(
        "  --arena-overflow <m> Behavior when arena exceeds memory: grow (default), trap, unchecked"
    );
    eprintln!("  --help, -h           Show this help");
    eprintln!("  --version, -V        Show version");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  tscc compile player.ts");
    eprintln!("  tscc compile player.ts -o build/player.wasm");
    eprintln!("  tscc compile script.ts --host-module env --memory-pages 4");
}

fn run_compile(args: &[String]) {
    if args.is_empty() {
        eprintln!("error: missing input file");
        eprintln!("Usage: tscc compile <input.ts> [options]");
        process::exit(1);
    }

    let input_path = &args[0];
    let mut output_path: Option<String> = None;
    let mut host_module = "host".to_string();
    let mut memory_pages: u32 = 1;
    let mut debug = false;
    let mut arena_overflow = tscc::ArenaOverflow::default();

    // Parse remaining options
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: -o requires an output file path");
                    process::exit(1);
                }
                output_path = Some(args[i].clone());
            }
            "--host-module" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --host-module requires a module name");
                    process::exit(1);
                }
                host_module = args[i].clone();
            }
            "--memory-pages" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --memory-pages requires a number");
                    process::exit(1);
                }
                memory_pages = args[i].parse().unwrap_or_else(|_| {
                    eprintln!("error: --memory-pages must be a positive integer");
                    process::exit(1);
                });
            }
            "--debug" | "-g" => {
                debug = true;
            }
            "--arena-overflow" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --arena-overflow requires a mode (grow|trap|unchecked)");
                    process::exit(1);
                }
                arena_overflow = match args[i].as_str() {
                    "grow" => tscc::ArenaOverflow::Grow,
                    "trap" => tscc::ArenaOverflow::Trap,
                    "unchecked" => tscc::ArenaOverflow::Unchecked,
                    other => {
                        eprintln!(
                            "error: --arena-overflow must be one of grow|trap|unchecked, got '{other}'"
                        );
                        process::exit(1);
                    }
                };
            }
            other => {
                eprintln!("error: unknown option '{other}'");
                process::exit(1);
            }
        }
        i += 1;
    }

    let output_path = output_path.unwrap_or_else(|| {
        Path::new(input_path)
            .with_extension("wasm")
            .to_string_lossy()
            .into_owned()
    });

    let source = match fs::read_to_string(input_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read '{input_path}': {e}");
            process::exit(1);
        }
    };

    let options = tscc::CompileOptions {
        host_module,
        memory_pages,
        debug,
        filename: input_path.to_string(),
        arena_overflow,
        ..Default::default()
    };

    match tscc::compile(&source, &options) {
        Ok(wasm) => {
            if let Err(e) = fs::write(&output_path, &wasm) {
                eprintln!("error: cannot write '{output_path}': {e}");
                process::exit(1);
            }
            eprintln!(
                "compiled {} -> {} ({} bytes)",
                input_path,
                output_path,
                wasm.len()
            );
        }
        Err(e) => {
            eprint!("{}", tscc::error::format_error_with_context(&e, &source));
            process::exit(1);
        }
    }
}
