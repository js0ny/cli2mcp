//! Test-only process fixture, compiled by the integration harness with `rustc`.
//!
//! Modes are selected by the first argument; the remaining arguments are mode-specific.

use std::{
    io::{Read, Write},
    process::Command,
    thread::sleep,
    time::Duration,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or_default();

    match mode {
        "argv" => println!("{:?}", &args[1..]),
        "exit" => {
            let code: i32 = args[1].parse().expect("exit code");
            if !args[2].is_empty() {
                print!("{}", args[2]);
            }
            if !args[3].is_empty() {
                eprint!("{}", args[3]);
            }
            std::io::stdout().flush().unwrap();
            std::io::stderr().flush().unwrap();
            std::process::exit(code);
        }
        "invalid-utf8" => {
            std::io::stdout().write_all(&[0xff, 0xfe, b'a']).unwrap();
            std::io::stdout().flush().unwrap();
        }
        "large" => {
            let size: usize = args[1].parse().expect("size");
            let stdout = vec![b'a'; size];
            let stderr = vec![b'b'; size];
            std::io::stdout().write_all(&stdout).unwrap();
            std::io::stdout().flush().unwrap();
            std::io::stderr().write_all(&stderr).unwrap();
            std::io::stderr().flush().unwrap();
        }
        "large-fail" => {
            let size: usize = args[1].parse().expect("size");
            let stdout = vec![b'a'; size];
            let stderr = vec![b'b'; size];
            std::io::stdout().write_all(&stdout).unwrap();
            std::io::stdout().flush().unwrap();
            std::io::stderr().write_all(&stderr).unwrap();
            std::io::stderr().flush().unwrap();
            std::process::exit(5);
        }
        "env" => match std::env::var(&args[1]) {
            Ok(value) => println!("{value}"),
            Err(_) => println!("<unset>"),
        },
        "cwd" => println!("{}", std::env::current_dir().unwrap().display()),
        "stdin" => {
            let mut buffer = Vec::new();
            std::io::stdin().read_to_end(&mut buffer).unwrap();
            println!("eof:{}", buffer.len());
        }
        "hold" => {
            write_pid(&args[1]);
            sleep(Duration::from_millis(args[2].parse().expect("milliseconds")));
        }
        "signal-ready" => {
            write_pid(&args[1]);
            sleep(Duration::from_secs(60));
        }
        "timeout" => {
            write_pid(&args[1]);
            spawn_self(&["hold", &args[2], "60000"]);
            sleep(Duration::from_secs(60));
        }
        "leader-exits" => {
            spawn_self(&["hold", &args[2], "60000"]);
            write_pid(&args[1]);
        }
        "close-then-sleep" => {
            write_pid(&args[1]);
            close_std_streams();
            sleep(Duration::from_millis(args[2].parse().expect("milliseconds")));
        }
        other => {
            eprintln!("unknown fixture mode: {other}");
            std::process::exit(2);
        }
    }
}

fn write_pid(path: &str) {
    std::fs::write(path, std::process::id().to_string()).unwrap();
}

fn spawn_self(args: &[&str]) {
    Command::new(std::env::current_exe().unwrap())
        .args(args)
        .spawn()
        .unwrap();
}

fn close_std_streams() {
    #[cfg(unix)]
    unsafe {
        use std::os::fd::FromRawFd;
        drop(std::fs::File::from_raw_fd(1));
        drop(std::fs::File::from_raw_fd(2));
    }
}
