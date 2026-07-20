use std::{env, fs, io::Write, path::Path, thread};

const BACKGROUND_PRIVATE_OUTPUT: &str =
    match option_env!("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PRIVATE_OUTPUT") {
        Some(value) => value,
        None => "TERMINAL_E2E_BACKGROUND_PRIVATE_STDOUT_DO_NOT_EXPOSE",
    };

fn main() {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        None => run_foreground_fixture(),
        Some("background") => {
            assert!(
                arguments.next().is_none(),
                "background fixture does not accept additional arguments"
            );
            run_background_fixture();
        }
        Some(mode) => panic!("unsupported terminal fixture mode: {}", mode),
    }
}

fn run_foreground_fixture() {
    let directory = Path::new("generated");
    fs::create_dir_all(directory).expect("create generated directory");
    fs::write(
        directory.join("terminal-e2e.txt"),
        "TERMINAL_E2E_PRIVATE_FILE_DO_NOT_EXPOSE",
    )
    .expect("write terminal fixture output");
    println!("TERMINAL_E2E_PRIVATE_STDOUT_DO_NOT_EXPOSE");
}

fn run_background_fixture() {
    let directory = Path::new("generated");
    fs::create_dir_all(directory).expect("create generated directory");
    fs::write(directory.join("background-terminal-started.txt"), "started")
        .expect("write background fixture start marker");

    println!("{}", BACKGROUND_PRIVATE_OUTPUT);
    std::io::stdout()
        .flush()
        .expect("flush background fixture output");

    loop {
        thread::park();
    }
}
