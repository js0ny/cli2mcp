mod config;

fn main() {
    let cfg = config::load("./config.example.toml").unwrap();
    println!("{cfg:?}");
}
